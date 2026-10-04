// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! In-app, in-memory, single-node rate limiting.
//!
//! The IP-keyed tower layers judge unauthenticated requests before the handler;
//! [`KeyedRateLimiter`] is the in-handler limiter for keys a layer cannot see,
//! such as a normalized email or an account id.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::response::IntoResponse;
use governor::clock::{Clock, DefaultClock};
use governor::middleware::NoOpMiddleware;
use governor::state::keyed::DashMapStateStore;
use governor::{Quota, RateLimiter};
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_governor::{GovernorError, GovernorLayer};

use super::problem::ApiError;

/// Every this-many `check` calls, fully-replenished (stale) keys are swept
/// from the store. Without a sweep, each distinct key ever submitted is a
/// permanent map entry — a memory-exhaustion vector on a public endpoint.
const SWEEP_EVERY: u64 = 256;

/// Generic keyed GCRA limiter over string keys, with a stale-key sweep
/// piggybacked on traffic. The quota (burst + replenish period) is pinned by
/// each wrapper at construction. Extracted once a THIRD keyed in-handler
/// limiter appeared (three-strike DRY): the sweep, the `check` mapping, and the
/// quota plumbing live here exactly once, and `EmailRateLimiter`,
/// `FavoriteRateLimiter`, and `FavoritesReadRateLimiter` are thin wrappers that
/// differ only in their pinned quota and the semantic name of their key.
struct KeyedRateLimiter<C: Clock = DefaultClock> {
    limiter: RateLimiter<String, DashMapStateStore<String>, C, NoOpMiddleware<C::Instant>>,
    clock: C,
    checks: AtomicU64,
}

impl<C: Clock + Clone> KeyedRateLimiter<C> {
    fn new(burst: u32, replenish_period: Duration, clock: C) -> Self {
        let quota = Quota::with_period(replenish_period)
            .expect("period is non-zero")
            .allow_burst(NonZeroU32::new(burst).expect("burst is non-zero"));
        Self {
            limiter: RateLimiter::new(quota, DashMapStateStore::default(), clock.clone()),
            clock,
            checks: AtomicU64::new(0),
        }
    }

    /// Admits the request or returns the seconds until the window admits
    /// another (surfaced as `Retry-After`).
    fn check(&self, key: &str) -> Result<(), u64> {
        // Piggyback stale-key eviction on traffic: O(keys) once per
        // SWEEP_EVERY checks, so the map is bounded by live keys, not by
        // every key ever sprayed at the endpoint.
        if self.checks.fetch_add(1, Ordering::Relaxed) % SWEEP_EVERY == SWEEP_EVERY - 1 {
            self.limiter.retain_recent();
        }

        self.limiter.check_key(&key.to_owned()).map_err(|denied| {
            // Round up so the hint is never a useless zero.
            denied.wait_time_from(self.clock.now()).as_secs().max(1)
        })
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.limiter.len()
    }
}

/// Pinned quota: 3 magic-link requests per normalized email per 15 minutes
/// (GCRA: burst of 3, one cell replenished every 5 minutes).
const BURST: u32 = 3;
const REPLENISH_PERIOD: Duration = Duration::from_secs(15 * 60 / 3);

/// Keyed limiter over normalized email addresses (magic links + email change).
pub struct EmailRateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for EmailRateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(DefaultClock::default())
    }
}

impl<C: Clock + Clone> EmailRateLimiter<C> {
    fn with_clock(clock: C) -> Self {
        Self(KeyedRateLimiter::new(BURST, REPLENISH_PERIOD, clock))
    }

    /// Admits the request or returns the seconds until the window admits
    /// another (surfaced as `Retry-After`). Keyed on the normalized email.
    pub fn check(&self, email: &str) -> Result<(), u64> {
        self.0.check(email)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// Pinned quota for the interactive favorite toggle: a
/// burst of 20 favorite writes per account, one cell replenished every 3
/// seconds (~20/min). Sized so rapid manual toggling is never throttled while
/// automated abuse is still capped. Tunable, like [`BURST`]/[`REPLENISH_PERIOD`].
const FAVORITE_BURST: u32 = 20;
const FAVORITE_REPLENISH_PERIOD: Duration = Duration::from_secs(3);

/// Keyed limiter over account ids (their UUID string form) for the favorite
/// write path (PUT + DELETE). Favoriting is an authenticated
/// per-account action, so the honest key is the `account_id`, NOT the IP — a
/// shared NAT/proxy IP would otherwise collapse distinct users into one bucket.
pub struct FavoriteRateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for FavoriteRateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(DefaultClock::default())
    }
}

impl<C: Clock + Clone> FavoriteRateLimiter<C> {
    fn with_clock(clock: C) -> Self {
        Self(KeyedRateLimiter::new(
            FAVORITE_BURST,
            FAVORITE_REPLENISH_PERIOD,
            clock,
        ))
    }

    /// Admits the favorite write or returns the seconds until the window admits
    /// another (surfaced as `Retry-After`). Keyed on the account id string.
    pub fn check(&self, account_id: &str) -> Result<(), u64> {
        self.0.check(account_id)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// Pinned quota for the participant self-check-in write: a burst of 10
/// self-check-in/self-toggle actions per account, one cell
/// replenished every 3 seconds. Sized so a participant checking in, toggling
/// their staying status a few times, and checking out is never throttled, while
/// a scripted flood is capped. The staff `LogCheckIn` path stays UNGOVERNED
/// (operators legitimately burst-log). Tunable, same posture as the sibling
/// quotas.
const SELF_CHECK_IN_BURST: u32 = 10;
const SELF_CHECK_IN_REPLENISH_PERIOD: Duration = Duration::from_secs(3);

/// Keyed limiter over account ids for the participant self-check-in write path
/// Account-keyed, NOT IP-keyed: the self-check-in write is
/// authenticated, so keying by IP would collapse distinct NAT'd participants
/// (a club running a net from one shared connection) into one bucket — the same
/// reasoning the favorite-write limiter keys on `account_id`. A SEPARATE bucket
/// from every other limiter.
pub struct SelfCheckInRateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for SelfCheckInRateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(DefaultClock::default())
    }
}

impl<C: Clock + Clone> SelfCheckInRateLimiter<C> {
    fn with_clock(clock: C) -> Self {
        Self(KeyedRateLimiter::new(
            SELF_CHECK_IN_BURST,
            SELF_CHECK_IN_REPLENISH_PERIOD,
            clock,
        ))
    }

    /// Admits the self-check-in write or returns the seconds until the window
    /// admits another (surfaced as `Retry-After`). Keyed on the account id string.
    pub fn check(&self, account_id: &str) -> Result<(), u64> {
        self.0.check(account_id)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// Pinned quota for the authenticated favorites LIST read: a burst of 30 reads
/// per account, one cell replenished every 2 seconds
/// (~30/min). A "My Nets" list is opened far less often than a star is toggled,
/// so this modest per-account allowance is ample for real use while capping a
/// scripted scrape of an account's own list. A SEPARATE bucket from the
/// favorite-WRITE limiter so a read never spends the write budget. Tunable,
/// same posture as the other pinned quotas.
const FAVORITES_READ_BURST: u32 = 30;
const FAVORITES_READ_REPLENISH_PERIOD: Duration = Duration::from_secs(2);

/// Keyed limiter over account ids for the favorites LIST read (`GET
/// /api/favorites`). Account-keyed, NOT IP-keyed: the read is
/// authenticated and account-isolated, so keying by IP would collapse distinct
/// NAT'd users into one bucket — the same reasoning the favorite-write limiter
/// keys on `account_id`.
pub struct FavoritesReadRateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for FavoritesReadRateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(DefaultClock::default())
    }
}

impl<C: Clock + Clone> FavoritesReadRateLimiter<C> {
    fn with_clock(clock: C) -> Self {
        Self(KeyedRateLimiter::new(
            FAVORITES_READ_BURST,
            FAVORITES_READ_REPLENISH_PERIOD,
            clock,
        ))
    }

    /// Admits the favorites read or returns the seconds until the window admits
    /// another (surfaced as `Retry-After`). Keyed on the account id string.
    pub fn check(&self, account_id: &str) -> Result<(), u64> {
        self.0.check(account_id)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// Pinned quota for the per-account callbook lookup: a burst
/// of 30 lookups per account, one cell replenished every 2 seconds (~30/min).
/// Sized so a human logging a busy net (a lookup per new callsign) is never
/// throttled, while a scripted flood is capped — the limiter protects the
/// upstream QRZ/hamcall providers and the shared egress client. Over-threshold
/// DEGRADES to no-lookup (the service returns `None`); it never 429s, because
/// there is no HTTP endpoint here and a lookup must never block a check-in.
/// Tunable, same posture as the sibling quotas.
const LOOKUP_BURST: u32 = 30;
const LOOKUP_REPLENISH_PERIOD: Duration = Duration::from_secs(2);

/// Keyed limiter over account ids for the callbook lookup path. The
/// lookup is an authenticated per-account action, so the honest key is the
/// `account_id`, NOT the IP — the same reasoning every other in-handler limiter
/// here keys on `account_id`. A SEPARATE bucket from the favorites/self-check-in
/// limiters. Consumed by `crate::lookup::LookupService`, which maps a denial to
/// "skip the lookup, return `None`" rather than surfacing an error.
pub struct LookupRateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for LookupRateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(DefaultClock::default())
    }
}

impl<C: Clock + Clone> LookupRateLimiter<C> {
    fn with_clock(clock: C) -> Self {
        Self(KeyedRateLimiter::new(
            LOOKUP_BURST,
            LOOKUP_REPLENISH_PERIOD,
            clock,
        ))
    }

    /// Admits the lookup, or returns the seconds until the window admits another
    /// (the service treats ANY `Err` as "skip the lookup"). Keyed on the account
    /// id string.
    pub fn check(&self, account_id: &str) -> Result<(), u64> {
        self.0.check(account_id)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// Pinned quota for net creation: a burst of 12 creates
/// per account, one cell replenished every 60 seconds. Deliberately sized ABOVE
/// the default max-nets-per-user cap (7) — plus headroom for an operator who
/// raises that cap and a few create/retry attempts — so a legitimate user
/// filling their cap in one sitting is NEVER throttled, while a scripted flood
/// of net rows (the spam-net abuse this limiter bounds, on top of the hard active-
/// net cap) is still capped at ~12/min then one per minute. Account-keyed like
/// the other in-handler write limiters. Tunable, same posture as the siblings.
/// `pub` so `main` can boot-time-warn if an operator-configured
/// `MAX_NETS_PER_USER` sits at or above this burst —
/// see `config::cap_may_be_throttled_by_rate_limiter`.
pub const NET_CREATION_BURST: u32 = 12;
const NET_CREATION_REPLENISH_PERIOD: Duration = Duration::from_secs(60);

/// Keyed limiter over account ids for the net-creation write path (`POST
/// /api/net-definitions`). Account-keyed, NOT IP-keyed: net creation
/// is authenticated, so keying by IP would collapse distinct NAT'd users (a club
/// creating nets from one shared connection) into one bucket — the same
/// reasoning every other in-handler limiter here keys on `account_id`. A
/// SEPARATE bucket from every other limiter. Denial maps to
/// `ApiError::RateLimited` (429 + `Retry-After`) at the top of the handler.
pub struct NetCreationRateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for NetCreationRateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(DefaultClock::default())
    }
}

impl<C: Clock + Clone> NetCreationRateLimiter<C> {
    fn with_clock(clock: C) -> Self {
        Self(KeyedRateLimiter::new(
            NET_CREATION_BURST,
            NET_CREATION_REPLENISH_PERIOD,
            clock,
        ))
    }

    /// Admits the net-creation request or returns the seconds until the window
    /// admits another (surfaced as `Retry-After`). Keyed on the account id string.
    pub fn check(&self, account_id: &str) -> Result<(), u64> {
        self.0.check(account_id)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// Pinned quota for the authenticated email-change request: a burst of 5
/// requests per account, one cell replenished
/// every 10 minutes (~6/hour sustained). Sized so a user who mistypes the new
/// address a few times is never throttled, while one consented account cannot
/// drain the shared instance-wide send budget (general + reserve) on its own —
/// which it otherwise could, because the shared per-address quota is keyed on
/// `new_email` and rotating targets defeats it. Tunable, same posture as the
/// sibling quotas.
const EMAIL_CHANGE_BURST: u32 = 5;
const EMAIL_CHANGE_REPLENISH_PERIOD: Duration = Duration::from_secs(10 * 60);

/// Keyed limiter over account ids for the email-change request path (`POST
/// /api/accounts/me/email-change`). Account-keyed, NOT `new_email`-keyed or
/// IP-keyed: the caller is authenticated, and keying on the target would (a) be
/// the per-address quota again and (b) let the answer correlate with the
/// target. Checked FIRST in the handler, before the shared per-address and
/// instance-wide checks, so a throttled account spends no shared cell. A
/// SEPARATE bucket from every other limiter. Denial maps to
/// `ApiError::RateLimited` (429 + `Retry-After`) — honest, because the key is
/// the caller's own account and reveals nothing about `new_email`.
pub struct EmailChangeRateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for EmailChangeRateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(DefaultClock::default())
    }
}

impl<C: Clock + Clone> EmailChangeRateLimiter<C> {
    fn with_clock(clock: C) -> Self {
        Self(KeyedRateLimiter::new(
            EMAIL_CHANGE_BURST,
            EMAIL_CHANGE_REPLENISH_PERIOD,
            clock,
        ))
    }

    /// Admits the email-change request or returns the seconds until the window
    /// admits another (surfaced as `Retry-After`). Keyed on the account id string.
    pub fn check(&self, account_id: &str) -> Result<(), u64> {
        self.0.check(account_id)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// Pinned quota for the public abuse-report submission:
/// a burst of 5 reports per IP, one cell replenished every 60 seconds. Sized so
/// a genuine reporter filing a couple of reports is never throttled, while a
/// scripted flood of the public, unauthenticated write is capped (the same spam
/// class signup and net creation are hardened against). Tunable, same posture as the
/// sibling quotas.
const ABUSE_REPORT_BURST: u32 = 5;
const ABUSE_REPORT_REPLENISH_PERIOD: Duration = Duration::from_secs(60);

/// Keyed limiter over client IP for the public abuse-report write (`POST
/// /api/abuse-reports`). IP-keyed (NOT account-keyed): the report
/// affordance is public and the reporter may have no account, so the honest key
/// is the client IP — the same keying axis as the auth-route IP quota
/// (`ip_governor_layer`), but expressed through the in-handler
/// [`KeyedRateLimiter`] so it is testable via the bare `api_router` (which wires
/// no tower `ConnectInfo` layer) and shares the stale-key sweep. A SEPARATE
/// bucket from every other limiter. Denial maps to `ApiError::RateLimited` (429
/// + `Retry-After`).
pub struct AbuseReportRateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for AbuseReportRateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(DefaultClock::default())
    }
}

impl<C: Clock + Clone> AbuseReportRateLimiter<C> {
    fn with_clock(clock: C) -> Self {
        Self(KeyedRateLimiter::new(
            ABUSE_REPORT_BURST,
            ABUSE_REPORT_REPLENISH_PERIOD,
            clock,
        ))
    }

    /// Admits the report or returns the seconds until the window admits another
    /// (surfaced as `Retry-After`). Keyed on the client IP string.
    pub fn check(&self, ip: &str) -> Result<(), u64> {
        self.0.check(ip)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// The single key every magic-link send draws on. A keyed limiter with ONE
/// constant key is a shared, instance-wide GCRA bucket, which is exactly the
/// mechanism an instance-wide budget needs — reusing [`KeyedRateLimiter`] rather than adding
/// `RateLimiter::direct` as a second code path keeps the sweep, the quota
/// plumbing and the `Retry-After` mapping defined once.
const INSTANCE_KEY: &str = "instance";

/// Instance-wide cap on outbound magic-link email.
///
/// UNLIKE every sibling in this file, this limiter takes **no key**: one bucket
/// bounds total magic-link mail across ALL addresses. That is the whole point.
/// The shipped per-address quota ([`EmailRateLimiter`], 3 per address per 15
/// min) is independent per address, so cycling a harvested address list drives
/// unbounded third-party mail through the instance's SMTP relay — an abuse of
/// strangers AND an availability risk to this platform's own authentication,
/// since magic-link delivery is the sole auth path and a blocklisted sending
/// domain means nobody can sign in at all.
///
/// The two limiters are separate mechanisms and BOTH stay in force: the
/// per-address quota is checked first (a single-address abuser still gets an
/// honest 429), this one second. A denial here is NOT a 429 — the caller drops
/// the request onto the existing uniform `202 Accepted` path, issuing no token
/// and sending no mail, so a capped response stays indistinguishable from a
/// successful one.
///
/// **Two instances of this type make the cap a two-tier reserve.**
/// `create_magic_link` draws the GENERAL
/// pool for every request — exempt or not — and only when that pool is spent may
/// a proven-control address fall back to the smaller RESERVE pool. The earlier
/// shape let an exempt address bypass the bucket outright, which meant total
/// outbound mail was unbounded once an attacker had established exemptions (one
/// capped cell per catch-all address, then unlimited sends). Now total mail is
/// bounded by general + reserve unconditionally, while a returning user still
/// signs in under a campaign against unknown addresses — served by the reserve
/// rather than by an unbounded bypass. That last property is now conditional
/// rather than absolute: the reserve is finite, so an attacker who first proves
/// control of a handful of addresses can drain it too.
/// Two `KeyedRateLimiter`s over the same [`INSTANCE_KEY`] are still two
/// independent buckets, since each owns its own state store.
///
/// Also unlike its siblings, the quota is operator-configurable
/// (`MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR` for the general pool,
/// `MAGIC_LINK_RESERVE_SENDS_PER_HOUR` for the reserve) rather than a pinned
/// constant, so [`MagicLinkAggregateLimiter::per_hour`] takes the boot-resolved
/// value.
pub struct MagicLinkAggregateLimiter<C: Clock = DefaultClock>(KeyedRateLimiter<C>);

impl Default for MagicLinkAggregateLimiter<DefaultClock> {
    fn default() -> Self {
        // Single source of the documented default: the config resolver, whose
        // `None` arm IS the default (the `max_nets_per_user` posture).
        Self::per_hour(
            crate::config::resolve_magic_link_aggregate_sends_per_hour(None)
                .expect("None resolves to the default aggregate cap"),
        )
    }
}

impl MagicLinkAggregateLimiter<DefaultClock> {
    /// Builds the limiter for a budget of `sends_per_hour` magic-link emails
    /// across the whole instance. `main` passes the boot-resolved
    /// `MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR`; integration tests pin a tiny value
    /// so the cap is reachable in a handful of requests.
    ///
    /// `sends_per_hour` is the SUSTAINED rate, and it is also the burst: a fresh
    /// bucket starts full, so this pool admits `sends_per_hour` immediately plus
    /// up to another `sends_per_hour` over the following hour — up to 2N in the
    /// first hour after boot. Operator-facing docs must quote that burst figure,
    /// not the sustained one; see
    /// `config::DEFAULT_MAGIC_LINK_RESERVE_SENDS_PER_HOUR`.
    pub fn per_hour(sends_per_hour: u32) -> Self {
        Self::per_hour_with_clock(sends_per_hour, DefaultClock::default())
    }

    /// The RESERVE pool at its documented default budget. A separate bucket from
    /// [`Default::default`]'s general pool; `main` overrides it from the resolved
    /// `MAGIC_LINK_RESERVE_SENDS_PER_HOUR`.
    pub fn reserve_default() -> Self {
        // Single source of the documented default: the config resolver, whose
        // `None` arm IS the default.
        Self::per_hour(
            crate::config::resolve_magic_link_reserve_sends_per_hour(None)
                .expect("None resolves to the default reserve budget"),
        )
    }
}

impl<C: Clock + Clone> MagicLinkAggregateLimiter<C> {
    fn per_hour_with_clock(sends_per_hour: u32, clock: C) -> Self {
        // A zero budget would both divide by zero here and pin a burst the GCRA
        // quota rejects. Config resolution already refuses 0 as a hard boot
        // error, so this floor is unreachable defense-in-depth, not a silent
        // fallback for an operator's bad value.
        let sends_per_hour = sends_per_hour.max(1);
        // Dividing the DURATION (not the seconds) keeps sub-second precision for
        // budgets above 3600/hour, where an integer seconds division would
        // truncate the period to an invalid zero. See the caution on
        // [`READ_PER_SECOND`]: `Quota::with_period` wants a period, not a rate.
        let replenish_period = Duration::from_secs(3600) / sends_per_hour;
        Self(KeyedRateLimiter::new(
            sends_per_hour,
            replenish_period,
            clock,
        ))
    }

    /// Admits one magic-link send against the instance-wide budget, or returns
    /// the seconds until the budget admits another. The caller must NOT surface
    /// that hint: a `Retry-After` on this path would itself announce that a
    /// limit fired.
    pub fn check(&self) -> Result<(), u64> {
        self.0.check(INSTANCE_KEY)
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.0.tracked_keys()
    }
}

/// Auth-route IP quota: one token replenished
/// every 6 seconds, burst 10 — ~10 admissions/minute/IP once the burst is
/// spent. A blunt secondary defense behind the per-email quota.
///
/// CAUTION: this is a replenish PERIOD in seconds,
/// NOT a requests-per-second rate — `tower_governor`'s `GovernorConfigBuilder`
/// has no rate setter, only a period setter (its own `per_second` method is
/// misleadingly named; its doc says "the interval after which one element ...
/// is replenished in seconds"). Read this constant as "one token every 6s",
/// never as "6 requests/sec".
const AUTH_IP_REPLENISH_PERIOD: Duration = Duration::from_secs(6);
const AUTH_IP_BURST: u32 = 10;

/// Public-read IP quota: deliberately MORE generous than the auth
/// quota — in BOTH burst (30 > 10) and sustained throughput (~15/s > ~10/min).
/// A human browsing discovery, tweaking filters/sort, and opening several
/// permalinks generates bursts of reads, so the ceiling sits well above
/// interactive use while still capping a scraper. Tunable, same posture as
/// [`BURST`]/[`REPLENISH_PERIOD`]; candidate to lift into `config.rs` if an
/// operator ever needs to tune it without a rebuild (YAGNI for now — a pinned
/// constant matches every other limiter here). MUST stay larger than the auth
/// quota (enforced by a unit test below).
///
/// Unlike [`AUTH_IP_REPLENISH_PERIOD`], THIS constant is a genuine
/// requests-per-second rate, not a period — [`read_replenish_period`] does the
/// rate-to-period conversion `GovernorConfigBuilder` actually needs. Do not
/// pass this directly to a period setter: naively doing so (as an earlier
/// version of this code did) silently turns "15/s" into "one token every 15
/// seconds" (~4/min) — LESS generous than the auth quota, which inverts the
/// intent that public reads be the more generous of the two.
const READ_PER_SECOND: u32 = 15;
/// How many public reads one IP may make back to back before the sustained
/// rate applies.
pub const READ_BURST: u32 = 30;

/// Converts [`READ_PER_SECOND`] (a rate) into the replenish period
/// `GovernorConfigBuilder::period` wants (a duration per token) — see the
/// caution on [`READ_PER_SECOND`] for why this indirection exists.
fn read_replenish_period() -> Duration {
    Duration::from_secs(1) / READ_PER_SECOND
}

/// Blunt-instrument IP limit for the public auth routes (10/min/IP,
/// secondary defense behind the per-email quota). Errors keep the
/// problem+json contract.
///
/// Keyed via `SmartIpKeyExtractor` (`X-Forwarded-For`/`X-Real-Ip`, falling
/// back to the TCP peer): production traffic arrives through Caddy on the
/// tailnet, so the raw peer IP is always the proxy — keying on it would
/// collapse every real client into one shared bucket. The forwarded header
/// is trustworthy in that topology because the app is not directly
/// internet-reachable.
pub(crate) fn ip_governor_layer() -> GovernorLayer<
    SmartIpKeyExtractor,
    NoOpMiddleware<governor::clock::QuantaInstant>,
    axum::body::Body,
> {
    ip_governor_layer_with(
        AUTH_IP_REPLENISH_PERIOD,
        AUTH_IP_BURST,
        "ip rate limiter could not judge the request",
    )
}

/// Public-READ IP limit: the same tower `SmartIpKeyExtractor`
/// mechanism and problem+json error mapping as [`ip_governor_layer`], but a
/// separate governor with its own, more-generous quota ([`READ_PER_SECOND`]/
/// [`READ_BURST`]). Layered ONLY on the public read routes in
/// `api_router_ip_limited`, never on the bare `api_router` (its
/// `SmartIpKeyExtractor` would 500 without a `ConnectInfo`). Its bucket is
/// independent of the auth layer's — throttling reads never touches the auth
/// routes and vice versa.
pub(crate) fn read_ip_governor_layer() -> GovernorLayer<
    SmartIpKeyExtractor,
    NoOpMiddleware<governor::clock::QuantaInstant>,
    axum::body::Body,
> {
    ip_governor_layer_with(
        read_replenish_period(),
        READ_BURST,
        "read rate limiter could not judge the request",
    )
}

/// Shared builder for the two IP tower governors: identical `SmartIpKeyExtractor`
/// keying and problem+json error mapping, differing only in the quota
/// (replenish period + burst) and the internal-error message (three-strike DRY
/// — two identical factories). Takes the replenish PERIOD directly (a
/// `Duration`) rather than a rate, so a caller whose own constant is a rate
/// (like [`READ_PER_SECOND`]) must convert it first — see
/// [`read_replenish_period`].
fn ip_governor_layer_with(
    replenish_period: Duration,
    burst: u32,
    extract_error: &'static str,
) -> GovernorLayer<
    SmartIpKeyExtractor,
    NoOpMiddleware<governor::clock::QuantaInstant>,
    axum::body::Body,
> {
    let config = GovernorConfigBuilder::default()
        .period(replenish_period)
        .burst_size(burst)
        .key_extractor(SmartIpKeyExtractor)
        .finish()
        .expect("static governor config is valid");
    GovernorLayer::new(Arc::new(config)).error_handler(move |err| match err {
        GovernorError::TooManyRequests { wait_time, .. } => ApiError::RateLimited {
            retry_after_secs: wait_time,
        }
        .into_response(),
        GovernorError::UnableToExtractKey | GovernorError::Other { .. } => {
            ApiError::Internal(extract_error).into_response()
        }
    })
}

#[cfg(test)]
mod tests {
    use governor::clock::FakeRelativeClock;

    use super::*;

    fn fake_limiter() -> (EmailRateLimiter<FakeRelativeClock>, FakeRelativeClock) {
        let clock = FakeRelativeClock::default();
        (EmailRateLimiter::with_clock(clock.clone()), clock)
    }

    #[test]
    fn three_requests_pass_and_the_fourth_is_denied_with_a_retry_hint() {
        let (limiter, _clock) = fake_limiter();

        for n in 1..=3 {
            assert!(
                limiter.check("op@example.com").is_ok(),
                "request {n} within quota"
            );
        }
        let denied = limiter.check("op@example.com");
        let retry_after = denied.expect_err("fourth request must be denied");
        assert!(retry_after >= 1, "Retry-After must be a usable hint");
    }

    #[test]
    fn a_different_email_is_unaffected() {
        let (limiter, _clock) = fake_limiter();

        for _ in 0..3 {
            limiter.check("noisy@example.com").expect("within quota");
        }
        limiter.check("noisy@example.com").expect_err("over quota");

        assert!(limiter.check("quiet@example.com").is_ok());
    }

    #[test]
    fn stale_email_keys_are_evicted_once_their_window_has_passed() {
        let (limiter, clock) = fake_limiter();

        for n in 0..200 {
            limiter
                .check(&format!("spray{n}@example.com"))
                .expect("first request per email is within quota");
        }
        assert!(limiter.tracked_keys() >= 200, "keys accumulate while live");

        // Past the full window every sprayed key is reclaimable; later
        // traffic (here one busy email, so survivors stay countable) must
        // trigger the sweep rather than grow the map forever.
        clock.advance(Duration::from_secs(15 * 60 + 1));
        for _ in 0..300 {
            limiter.check("later@example.com").ok();
        }
        assert!(
            limiter.tracked_keys() < 200,
            "stale keys must be evicted, got {}",
            limiter.tracked_keys()
        );
    }

    #[test]
    fn the_window_admits_requests_again_after_it_resets() {
        let (limiter, clock) = fake_limiter();

        for _ in 0..3 {
            limiter.check("op@example.com").expect("within quota");
        }
        limiter.check("op@example.com").expect_err("over quota");

        clock.advance(Duration::from_secs(15 * 60));
        for n in 1..=3 {
            assert!(
                limiter.check("op@example.com").is_ok(),
                "request {n} after the window reset"
            );
        }
    }

    fn fake_favorite_limiter() -> (FavoriteRateLimiter<FakeRelativeClock>, FakeRelativeClock) {
        let clock = FakeRelativeClock::default();
        (FavoriteRateLimiter::with_clock(clock.clone()), clock)
    }

    #[test]
    fn favorite_burst_passes_and_the_next_is_denied_with_a_retry_hint() {
        let (limiter, _clock) = fake_favorite_limiter();
        let account = "018f0000-0000-7000-8000-000000000001";

        for n in 1..=FAVORITE_BURST {
            assert!(
                limiter.check(account).is_ok(),
                "favorite {n} within the burst quota"
            );
        }
        let retry_after = limiter
            .check(account)
            .expect_err("the write past the burst must be denied");
        assert!(retry_after >= 1, "Retry-After must be a usable hint");
    }

    #[test]
    fn a_different_account_is_unaffected_by_a_throttled_one() {
        let (limiter, _clock) = fake_favorite_limiter();
        let noisy = "018f0000-0000-7000-8000-0000000000aa";
        let quiet = "018f0000-0000-7000-8000-0000000000bb";

        for _ in 0..FAVORITE_BURST {
            limiter.check(noisy).expect("within quota");
        }
        limiter.check(noisy).expect_err("over quota");

        assert!(
            limiter.check(quiet).is_ok(),
            "a different account has its own bucket"
        );
    }

    #[test]
    fn stale_favorite_keys_are_evicted_once_their_window_has_passed() {
        // Mirrors the email limiter's sweep test: the sweep logic is shared
        // (same SWEEP_EVERY constant, same retain_recent() call), so a
        // parity test here closes the coverage gap this sibling shipped
        // without.
        let (limiter, clock) = fake_favorite_limiter();

        for n in 0..200 {
            let account = format!("018f0000-0000-7000-8000-{n:012}");
            limiter
                .check(&account)
                .expect("first write per account is within quota");
        }

        clock.advance(FAVORITE_REPLENISH_PERIOD * FAVORITE_BURST + Duration::from_secs(1));
        for _ in 0..300 {
            limiter.check("018f0000-0000-7000-8000-999999999999").ok();
        }
        assert!(
            limiter.tracked_keys() < 200,
            "stale account keys must be evicted, got {}",
            limiter.tracked_keys()
        );
    }

    #[test]
    fn the_favorite_window_admits_writes_again_after_it_resets() {
        let (limiter, clock) = fake_favorite_limiter();
        let account = "018f0000-0000-7000-8000-0000000000cc";

        for _ in 0..FAVORITE_BURST {
            limiter.check(account).expect("within quota");
        }
        limiter.check(account).expect_err("over quota");

        // A full burst's worth of replenishment refills the bucket.
        clock.advance(FAVORITE_REPLENISH_PERIOD * FAVORITE_BURST);
        assert!(
            limiter.check(account).is_ok(),
            "the window admits writes again after it resets"
        );
    }

    fn fake_email_change_limiter() -> (EmailChangeRateLimiter<FakeRelativeClock>, FakeRelativeClock)
    {
        let clock = FakeRelativeClock::default();
        (EmailChangeRateLimiter::with_clock(clock.clone()), clock)
    }

    #[test]
    fn email_change_burst_passes_and_the_next_is_denied_with_a_retry_hint() {
        let (limiter, _clock) = fake_email_change_limiter();
        let account = "018f0000-0000-7000-8000-000000000e01";

        for n in 1..=EMAIL_CHANGE_BURST {
            assert!(
                limiter.check(account).is_ok(),
                "email-change request {n} within the burst quota"
            );
        }
        let retry_after = limiter
            .check(account)
            .expect_err("the request past the burst must be denied");
        assert!(retry_after >= 1, "Retry-After must be a usable hint");
    }

    #[test]
    fn a_different_account_is_unaffected_by_a_throttled_email_changer() {
        let (limiter, _clock) = fake_email_change_limiter();
        let noisy = "018f0000-0000-7000-8000-000000000e0a";
        let quiet = "018f0000-0000-7000-8000-000000000e0b";

        for _ in 0..EMAIL_CHANGE_BURST {
            limiter.check(noisy).expect("within quota");
        }
        limiter.check(noisy).expect_err("over quota");

        assert!(
            limiter.check(quiet).is_ok(),
            "a different account has its own bucket"
        );
    }

    #[test]
    fn stale_email_change_keys_are_evicted_once_their_window_has_passed() {
        let (limiter, clock) = fake_email_change_limiter();

        for n in 0..200 {
            let account = format!("018f0000-0000-7000-8000-{n:012}");
            limiter
                .check(&account)
                .expect("first request per account is within quota");
        }

        clock.advance(EMAIL_CHANGE_REPLENISH_PERIOD * EMAIL_CHANGE_BURST + Duration::from_secs(1));
        for _ in 0..300 {
            limiter.check("018f0000-0000-7000-8000-999999999999").ok();
        }
        assert!(
            limiter.tracked_keys() < 200,
            "stale account keys must be evicted, got {}",
            limiter.tracked_keys()
        );
    }

    #[test]
    fn the_email_change_window_admits_requests_again_after_it_resets() {
        let (limiter, clock) = fake_email_change_limiter();
        let account = "018f0000-0000-7000-8000-000000000e0c";

        for _ in 0..EMAIL_CHANGE_BURST {
            limiter.check(account).expect("within quota");
        }
        limiter.check(account).expect_err("over quota");

        clock.advance(EMAIL_CHANGE_REPLENISH_PERIOD * EMAIL_CHANGE_BURST);
        assert!(
            limiter.check(account).is_ok(),
            "the window admits requests again after it resets"
        );
    }

    fn fake_abuse_report_limiter() -> (AbuseReportRateLimiter<FakeRelativeClock>, FakeRelativeClock)
    {
        let clock = FakeRelativeClock::default();
        (AbuseReportRateLimiter::with_clock(clock.clone()), clock)
    }

    #[test]
    fn abuse_report_burst_passes_and_the_next_is_denied_with_a_retry_hint() {
        let (limiter, _clock) = fake_abuse_report_limiter();
        let ip = "203.0.113.7";

        for n in 1..=ABUSE_REPORT_BURST {
            assert!(
                limiter.check(ip).is_ok(),
                "abuse report {n} within the burst quota"
            );
        }
        let retry_after = limiter
            .check(ip)
            .expect_err("the report past the burst must be denied");
        assert!(retry_after >= 1, "Retry-After must be a usable hint");
    }

    #[test]
    fn a_different_ip_is_unaffected_by_a_throttled_reporter() {
        let (limiter, _clock) = fake_abuse_report_limiter();
        let noisy = "203.0.113.7";
        let quiet = "198.51.100.9";

        for _ in 0..ABUSE_REPORT_BURST {
            limiter.check(noisy).expect("within quota");
        }
        limiter.check(noisy).expect_err("over quota");

        assert!(
            limiter.check(quiet).is_ok(),
            "a different IP has its own report bucket"
        );
    }

    #[test]
    fn stale_abuse_report_keys_are_evicted_once_their_window_has_passed() {
        let (limiter, clock) = fake_abuse_report_limiter();

        for n in 0..200 {
            limiter
                .check(&format!("203.0.{}.{}", n / 256, n % 256))
                .expect("first report per IP is within quota");
        }

        clock.advance(ABUSE_REPORT_REPLENISH_PERIOD * ABUSE_REPORT_BURST + Duration::from_secs(1));
        for _ in 0..300 {
            limiter.check("198.51.100.42").ok();
        }
        assert!(
            limiter.tracked_keys() < 200,
            "stale IP keys must be evicted, got {}",
            limiter.tracked_keys()
        );
    }

    #[test]
    fn the_read_ip_quota_is_more_generous_than_the_auth_ip_quota() {
        // Regression guard: `AUTH_IP_REPLENISH_PERIOD`
        // is a period (bigger = slower refill) while `READ_PER_SECOND` is a rate
        // (bigger = faster refill) — comparing the raw constants directly would
        // compare different units. Compare the derived sustained per-minute rate
        // instead, alongside the burst comparison.
        let auth_per_minute = 60.0 / AUTH_IP_REPLENISH_PERIOD.as_secs_f64();
        let read_per_minute = 60.0 / read_replenish_period().as_secs_f64();

        // Both sides are compile-time constants, so clippy (rightly) flags a
        // runtime `assert!` on them as pointless — but this is exactly the
        // regression this test exists to catch, so pin it as a real
        // compile-time assertion instead of silencing the lint.
        const {
            assert!(
                READ_BURST > AUTH_IP_BURST,
                "the read burst must exceed the auth burst"
            )
        };
        assert!(
            read_per_minute > auth_per_minute,
            "the read sustained rate ({read_per_minute}/min) must exceed the auth \
             sustained rate ({auth_per_minute}/min)"
        );
    }

    fn fake_self_check_in_limiter() -> (SelfCheckInRateLimiter<FakeRelativeClock>, FakeRelativeClock)
    {
        let clock = FakeRelativeClock::default();
        (SelfCheckInRateLimiter::with_clock(clock.clone()), clock)
    }

    #[test]
    fn self_check_in_burst_passes_and_the_next_is_denied_with_a_retry_hint() {
        let (limiter, _clock) = fake_self_check_in_limiter();
        let account = "018f0000-0000-7000-8000-000000000001";

        for n in 1..=SELF_CHECK_IN_BURST {
            assert!(
                limiter.check(account).is_ok(),
                "self-check-in {n} within the burst quota"
            );
        }
        let retry_after = limiter
            .check(account)
            .expect_err("the write past the burst must be denied");
        assert!(retry_after >= 1, "Retry-After must be a usable hint");
    }

    #[test]
    fn a_different_account_is_unaffected_by_a_throttled_self_check_in() {
        let (limiter, _clock) = fake_self_check_in_limiter();
        let noisy = "018f0000-0000-7000-8000-0000000000aa";
        let quiet = "018f0000-0000-7000-8000-0000000000bb";

        for _ in 0..SELF_CHECK_IN_BURST {
            limiter.check(noisy).expect("within quota");
        }
        limiter.check(noisy).expect_err("over quota");

        assert!(
            limiter.check(quiet).is_ok(),
            "a different account has its own self-check-in bucket"
        );
    }

    #[test]
    fn stale_self_check_in_keys_are_evicted_once_their_window_has_passed() {
        let (limiter, clock) = fake_self_check_in_limiter();

        for n in 0..200 {
            let account = format!("018f0000-0000-7000-8000-{n:012}");
            limiter
                .check(&account)
                .expect("first write per account is within quota");
        }

        clock
            .advance(SELF_CHECK_IN_REPLENISH_PERIOD * SELF_CHECK_IN_BURST + Duration::from_secs(1));
        for _ in 0..300 {
            limiter.check("018f0000-0000-7000-8000-999999999999").ok();
        }
        assert!(
            limiter.tracked_keys() < 200,
            "stale account keys must be evicted, got {}",
            limiter.tracked_keys()
        );
    }

    fn fake_favorites_read_limiter() -> (
        FavoritesReadRateLimiter<FakeRelativeClock>,
        FakeRelativeClock,
    ) {
        let clock = FakeRelativeClock::default();
        (FavoritesReadRateLimiter::with_clock(clock.clone()), clock)
    }

    #[test]
    fn favorites_read_burst_passes_and_the_next_is_denied_with_a_retry_hint() {
        let (limiter, _clock) = fake_favorites_read_limiter();
        let account = "018f0000-0000-7000-8000-000000000001";

        for n in 1..=FAVORITES_READ_BURST {
            assert!(
                limiter.check(account).is_ok(),
                "favorites read {n} within the burst quota"
            );
        }
        let retry_after = limiter
            .check(account)
            .expect_err("the read past the burst must be denied");
        assert!(retry_after >= 1, "Retry-After must be a usable hint");
    }

    #[test]
    fn a_different_account_is_unaffected_by_a_throttled_reader() {
        let (limiter, _clock) = fake_favorites_read_limiter();
        let noisy = "018f0000-0000-7000-8000-0000000000aa";
        let quiet = "018f0000-0000-7000-8000-0000000000bb";

        for _ in 0..FAVORITES_READ_BURST {
            limiter.check(noisy).expect("within quota");
        }
        limiter.check(noisy).expect_err("over quota");

        assert!(
            limiter.check(quiet).is_ok(),
            "a different account has its own read bucket"
        );
    }

    #[test]
    fn stale_favorites_read_keys_are_evicted_once_their_window_has_passed() {
        let (limiter, clock) = fake_favorites_read_limiter();

        for n in 0..200 {
            let account = format!("018f0000-0000-7000-8000-{n:012}");
            limiter
                .check(&account)
                .expect("first read per account is within quota");
        }

        clock.advance(
            FAVORITES_READ_REPLENISH_PERIOD * FAVORITES_READ_BURST + Duration::from_secs(1),
        );
        for _ in 0..300 {
            limiter.check("018f0000-0000-7000-8000-999999999999").ok();
        }
        assert!(
            limiter.tracked_keys() < 200,
            "stale account keys must be evicted, got {}",
            limiter.tracked_keys()
        );
    }

    fn fake_lookup_limiter() -> (LookupRateLimiter<FakeRelativeClock>, FakeRelativeClock) {
        let clock = FakeRelativeClock::default();
        (LookupRateLimiter::with_clock(clock.clone()), clock)
    }

    #[test]
    fn lookup_burst_passes_and_the_next_is_denied() {
        let (limiter, _clock) = fake_lookup_limiter();
        let account = "018f0000-0000-7000-8000-000000000001";

        for n in 1..=LOOKUP_BURST {
            assert!(
                limiter.check(account).is_ok(),
                "lookup {n} within the burst quota"
            );
        }
        let retry_after = limiter
            .check(account)
            .expect_err("the lookup past the burst must be denied");
        assert!(retry_after >= 1, "Retry-After must be a usable hint");
    }

    #[test]
    fn a_different_account_is_unaffected_by_a_throttled_looker() {
        let (limiter, _clock) = fake_lookup_limiter();
        let noisy = "018f0000-0000-7000-8000-0000000000aa";
        let quiet = "018f0000-0000-7000-8000-0000000000bb";

        for _ in 0..LOOKUP_BURST {
            limiter.check(noisy).expect("within quota");
        }
        limiter.check(noisy).expect_err("over quota");

        assert!(
            limiter.check(quiet).is_ok(),
            "a different account has its own lookup bucket"
        );
    }

    #[test]
    fn the_lookup_window_admits_requests_again_after_it_resets() {
        let (limiter, clock) = fake_lookup_limiter();
        let account = "018f0000-0000-7000-8000-0000000000cc";

        for _ in 0..LOOKUP_BURST {
            limiter.check(account).expect("within quota");
        }
        limiter.check(account).expect_err("over quota");

        clock.advance(LOOKUP_REPLENISH_PERIOD * LOOKUP_BURST);
        assert!(
            limiter.check(account).is_ok(),
            "the window admits lookups again after it resets"
        );
    }

    #[test]
    fn stale_lookup_keys_are_evicted_once_their_window_has_passed() {
        let (limiter, clock) = fake_lookup_limiter();

        for n in 0..200 {
            let account = format!("018f0000-0000-7000-8000-{n:012}");
            limiter
                .check(&account)
                .expect("first lookup per account is within quota");
        }

        clock.advance(LOOKUP_REPLENISH_PERIOD * LOOKUP_BURST + Duration::from_secs(1));
        for _ in 0..300 {
            limiter.check("018f0000-0000-7000-8000-999999999999").ok();
        }
        assert!(
            limiter.tracked_keys() < 200,
            "stale account keys must be evicted, got {}",
            limiter.tracked_keys()
        );
    }

    fn fake_net_creation_limiter() -> (NetCreationRateLimiter<FakeRelativeClock>, FakeRelativeClock)
    {
        let clock = FakeRelativeClock::default();
        (NetCreationRateLimiter::with_clock(clock.clone()), clock)
    }

    #[test]
    fn net_creation_burst_passes_and_the_next_is_denied_with_a_retry_hint() {
        let (limiter, _clock) = fake_net_creation_limiter();
        let account = "018f0000-0000-7000-8000-000000000001";

        for n in 1..=NET_CREATION_BURST {
            assert!(
                limiter.check(account).is_ok(),
                "net creation {n} within the burst quota"
            );
        }
        let retry_after = limiter
            .check(account)
            .expect_err("the create past the burst must be denied");
        assert!(retry_after >= 1, "Retry-After must be a usable hint");
    }

    #[test]
    fn a_different_account_is_unaffected_by_a_throttled_creator() {
        let (limiter, _clock) = fake_net_creation_limiter();
        let noisy = "018f0000-0000-7000-8000-0000000000aa";
        let quiet = "018f0000-0000-7000-8000-0000000000bb";

        for _ in 0..NET_CREATION_BURST {
            limiter.check(noisy).expect("within quota");
        }
        limiter.check(noisy).expect_err("over quota");

        assert!(
            limiter.check(quiet).is_ok(),
            "a different account has its own net-creation bucket"
        );
    }

    #[test]
    fn the_net_creation_window_admits_creates_again_after_it_resets() {
        let (limiter, clock) = fake_net_creation_limiter();
        let account = "018f0000-0000-7000-8000-0000000000cc";

        for _ in 0..NET_CREATION_BURST {
            limiter.check(account).expect("within quota");
        }
        limiter.check(account).expect_err("over quota");

        clock.advance(NET_CREATION_REPLENISH_PERIOD * NET_CREATION_BURST);
        assert!(
            limiter.check(account).is_ok(),
            "the window admits creates again after it resets"
        );
    }

    fn fake_aggregate_limiter(
        sends_per_hour: u32,
    ) -> (
        MagicLinkAggregateLimiter<FakeRelativeClock>,
        FakeRelativeClock,
    ) {
        let clock = FakeRelativeClock::default();
        (
            MagicLinkAggregateLimiter::per_hour_with_clock(sends_per_hour, clock.clone()),
            clock,
        )
    }

    #[test]
    fn the_aggregate_budget_admits_the_configured_hourly_burst_then_denies() {
        let (limiter, _clock) = fake_aggregate_limiter(5);

        for n in 1..=5 {
            assert!(limiter.check().is_ok(), "send {n} within the hourly budget");
        }
        let retry_after = limiter
            .check()
            .expect_err("the send past the hourly budget must be denied");
        assert!(retry_after >= 1, "the retry hint must be usable");
    }

    #[test]
    fn the_aggregate_budget_tracks_exactly_one_instance_wide_bucket() {
        // THE property that distinguishes this limiter from every keyed sibling:
        // there is ONE bucket for the whole instance, so a harvested list cannot
        // buy itself more sends by cycling addresses. `check()` taking no key is
        // how that is enforced; this asserts the consequence the burst test
        // cannot — that however many sends are drawn, the underlying store still
        // holds a single key. Reintroducing per-caller keying (the regression
        // this guards) would show up here as more than one tracked key.
        let (limiter, _clock) = fake_aggregate_limiter(3);

        for _ in 0..3 {
            limiter.check().expect("within the shared instance budget");
        }
        limiter.check().expect_err("the one shared budget is spent");

        assert_eq!(
            limiter.tracked_keys(),
            1,
            "every send must draw on ONE instance-wide bucket"
        );
    }

    #[test]
    fn the_general_pool_and_the_reserve_are_independent_buckets() {
        // The two-tier cap is two INSTANCES of this type,
        // both over the same constant key. That only bounds the total if each
        // owns its own state store — otherwise the reserve would already be
        // spent by the general pool's traffic and the reserve would fail under
        // saturation.
        let (general, _general_clock) = fake_aggregate_limiter(2);
        let (reserve, _reserve_clock) = fake_aggregate_limiter(2);

        for _ in 0..2 {
            general.check().expect("within the general budget");
        }
        general.check().expect_err("the general budget is spent");

        assert!(
            reserve.check().is_ok(),
            "the reserve keeps its own budget when the general pool is exhausted"
        );
    }

    #[test]
    fn the_aggregate_window_admits_sends_again_after_it_replenishes() {
        let (limiter, clock) = fake_aggregate_limiter(6);

        for _ in 0..6 {
            limiter.check().expect("within the hourly budget");
        }
        limiter.check().expect_err("over the hourly budget");

        // 6/hour ⇒ one cell every 10 minutes.
        clock.advance(Duration::from_secs(600));
        assert!(
            limiter.check().is_ok(),
            "the window admits a send again once a cell replenishes"
        );
    }

    #[test]
    fn stale_net_creation_keys_are_evicted_once_their_window_has_passed() {
        let (limiter, clock) = fake_net_creation_limiter();

        for n in 0..200 {
            let account = format!("018f0000-0000-7000-8000-{n:012}");
            limiter
                .check(&account)
                .expect("first create per account is within quota");
        }

        clock.advance(NET_CREATION_REPLENISH_PERIOD * NET_CREATION_BURST + Duration::from_secs(1));
        for _ in 0..300 {
            limiter.check("018f0000-0000-7000-8000-999999999999").ok();
        }
        assert!(
            limiter.tracked_keys() < 200,
            "stale account keys must be evicted, got {}",
            limiter.tracked_keys()
        );
    }
}
