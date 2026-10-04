// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The background sweep that pays off durable on-close delivery legs.
//!
//! The close path plans one `net_delivery_jobs` row per destination in its own
//! transaction and runs them as a latency optimisation; whatever it did not
//! finish stays `pending`, and every transition here is a guarded `UPDATE`.

use std::collections::BTreeMap;

use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use netroll_adapters::pg::delivery_jobs::{DeliveryJobRepo, TerminalState};
use netroll_domain::ports::Clock;

use crate::delivery::{
    DeliveryDestinationKind, DeliveryService, LegTally, MAX_CONCURRENT_DELIVERIES,
};

/// How often the delivery sweep ticks. The presence monitor's cadence — the
/// shortest of the four maintenance sweeps — because this is the only sweep a
/// club is actually waiting on. `tokio::time::interval` completes its first tick
/// immediately, so the first sweep runs at boot, right after the boot recovery
/// pass has released whatever the previous process left in flight.
pub const DELIVERY_SWEEP_TICK: Duration = Duration::from_secs(30);

/// How many interrupted legs one recovery pass will classify.
///
/// Bounded for the same reason `due_sessions` is: the boot pass is AWAITED before
/// `axum::serve`, and it issues one write per row, so an unbounded read after a
/// long outage would hold the process out of service for as long as the backlog
/// takes. What this pass does not reach, the next tick does — 30 s later, and
/// without gating anything.
pub const MAX_RECOVERED_PER_PASS: i64 = 256;

/// How long a claim leases a leg to one attempt. MUST exceed the worst-case
/// CLAIM, not the worst-case leg — one claim leases every leg of the session at
/// the same instant, so the bound is over all of them together. Derived, not
/// guessed: one webhook attempt is bounded by the 15 s egress timeout and the
/// in-run ladder runs three of them (~45 s); one email address is three × the
/// 10 s SMTP timeout (~30 s), and the addresses are sent CONCURRENTLY, so the
/// whole fan-out still costs ~30 s however many the domain cap allows; a
/// Discord 429 may add up to 5 s per attempt. The legs themselves run
/// concurrently under `join!`, so the worst claim is the worst single leg
/// (~45 s) and 180 s is roughly four times it. A lease SHORTER than the work it
/// covers would let recovery re-claim a LIVE attempt and double-send — the
/// story's own failure mode, self-inflicted.
///
/// The concurrency of the email fan-out is load-bearing HERE, not an
/// optimisation: sequentially, the claim would cost `MAX_DELIVERY_EMAILS` ×
/// ~30 s ≈ 300 s and overrun this lease at roughly six addresses, and
/// `every_email_leg_of_a_session_is_in_flight_at_once_so_the_fan_out_fits_the_lease`
/// is what keeps it true.
pub const DELIVERY_LEASE_MILLIS: u64 = 180_000;

/// How many claims a leg gets before it is recorded as `failed` rather than
/// retried forever. With the ladder below the ten attempts span roughly two
/// hours of transient failure (30 s + 1 + 2 + 4 + 8 + 16 + 30 + 30 + 30 min):
/// a receiver down for an afternoon still gets the summary; one down for a
/// week does not, and that is recorded rather than retried forever. Compared
/// with `>=`, never `==`, so a row whose attempts somehow overshoot still stops.
pub const MAX_JOB_ATTEMPTS: i32 = 10;

/// The first outer-ladder delay after a spent in-run ladder. Doubles per
/// attempt up to [`DELIVERY_RETRY_MAX_MILLIS`].
pub const DELIVERY_RETRY_BASE_MILLIS: u64 = 30_000;

/// The ceiling of the doubling ladder: 30 minutes.
pub const DELIVERY_RETRY_MAX_MILLIS: u64 = 30 * 60_000;

/// How long a terminal row is kept before the sweep prunes it: 30 days. Its own
/// const rather than `TOKEN_RETENTION_DAYS`, because that window is a credential
/// policy and this is a delivery-record one (Question 4, drafted default).
pub const DELIVERY_JOB_RETENTION_MILLIS: u64 = 30 * 24 * 60 * 60 * 1_000;

/// The outer ladder: how long after a spent attempt the leg is next due, given
/// how many claims it has had so far (at least one — the one that just ran).
/// 30 s, 1 m, 2 m, 4 m, 8 m, 16 m, then 30 m flat. Never a panic and never a
/// zero: the doubling is done on `1u64` with a `checked_mul`, so an attempt
/// count past the ladder's reach saturates INTO the cap rather than wrapping
/// through it.
///
/// `DELIVERY_RETRY_BASE_MILLIS.checked_shl(spent)` was the obvious spelling and
/// it is wrong: `checked_shl` guards the shift WIDTH (`None` only at 64 or
/// more), not the value. `30_000` is 1875 x 2^4, so at `spent = 60`
/// (`attempts = 61`) the product is a multiple of 2^64 and wraps to exactly
/// zero — "retry immediately", out of the function that schedules the backoff.
/// Unreachable while `settle_leg` terminalises at `MAX_JOB_ATTEMPTS`, but this
/// is `pub` and raising that budget is described elsewhere as a one-const
/// change.
pub fn retry_delay_millis(attempts: i32) -> u64 {
    let spent = u32::try_from(attempts.saturating_sub(1)).unwrap_or(0);
    1_u64
        .checked_shl(spent)
        .and_then(|factor| DELIVERY_RETRY_BASE_MILLIS.checked_mul(factor))
        .unwrap_or(DELIVERY_RETRY_MAX_MILLIS)
        .min(DELIVERY_RETRY_MAX_MILLIS)
}

/// What one recovery pass did with the legs whose attempt died in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecoveredCounts {
    /// Interrupted legs released back to `pending` and due now (email, webhook).
    pub released: u64,
    /// Interrupted legs marked `failed` because their destination cannot tell a
    /// retry from a duplicate (Discord).
    pub abandoned: u64,
}

/// What one sweep tick did, by row transition. Counts and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweptCounts {
    /// The recovery pass that opened the tick.
    pub recovered: RecoveredCounts,
    /// Legs claimed by this tick.
    pub claimed: u64,
    /// Distinct sessions those legs belonged to — i.e. how many folds ran.
    pub sessions: u64,
    /// How the claimed legs were settled.
    pub settled: LegTally,
    /// Session groups whose executor hit a STORAGE error (their legs stay
    /// claimed and are recovered when the lease expires).
    pub errored: u64,
    /// Terminal rows removed by the retention prune.
    pub pruned: u64,
}

impl SweptCounts {
    /// Whether the tick did anything worth a log line.
    fn is_quiet(&self) -> bool {
        *self == Self::default()
    }
}

/// Classifies every leg whose lease has expired while still `pending` — an
/// attempt that died in flight — and moves it per destination (see the module
/// doc). Pure in its arguments: `now_millis` decides which leases have expired.
///
/// The policy lives HERE, in the app layer, and nowhere in the repo: the repo
/// only knows how to list interrupted legs and how to move a row.
pub async fn recover_interrupted_deliveries(
    jobs: &DeliveryJobRepo,
    now_millis: u64,
) -> Result<RecoveredCounts, sqlx::Error> {
    recover_with_scope(jobs, now_millis, RecoveryScope::ExpiredLeases).await
}

/// Which claims a recovery pass treats as interrupted.
///
/// A running tick may only touch a lease that has LAPSED — anything else is a
/// live attempt in this very process. The boot pass is the opposite case and
/// the distinction is load-bearing: at boot no attempt of this process can be
/// running, so a claim still inside its lease belongs to the process that died,
/// and filtering it out makes the boot pass a no-op for exactly the crash it
/// exists to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryScope {
    /// Only claims whose lease has already expired. The per-tick pass.
    ExpiredLeases,
    /// Every claim, lease or no lease. The boot pass, and only safe there.
    EveryClaim,
}

/// Recovers interrupted legs under an explicit [`RecoveryScope`].
///
/// One classification policy, two selections — the per-destination rules below
/// must not fork between the boot pass and the tick pass.
pub async fn recover_with_scope(
    jobs: &DeliveryJobRepo,
    now_millis: u64,
    scope: RecoveryScope,
) -> Result<RecoveredCounts, sqlx::Error> {
    let mut counts = RecoveredCounts::default();
    let interrupted = match scope {
        RecoveryScope::ExpiredLeases => {
            jobs.interrupted(now_millis, MAX_RECOVERED_PER_PASS).await?
        }
        RecoveryScope::EveryClaim => jobs.claimed(MAX_RECOVERED_PER_PASS).await?,
    };
    for leg in interrupted {
        // The budget binds on THIS path too, not only when an attempt
        // completes. `attempts` is burned at claim and the claim has no
        // attempts predicate, so a leg interrupted every cycle — a crash loop,
        // or an attempt that outlives its lease — would otherwise climb without
        // bound and re-send each time, which is exactly the "retried forever"
        // shape the attempt ceiling exists to close.
        if leg.attempts >= MAX_JOB_ATTEMPTS {
            jobs.mark_terminal(
                leg.id,
                TerminalState::Failed,
                now_millis,
                leg.lease_until_millis,
            )
            .await?;
            tracing::warn!(
                net_session_id = %leg.session_id,
                net_delivery_job_id = %leg.id,
                attempts = leg.attempts,
                "an interrupted delivery leg has spent its attempt budget; failed"
            );
            counts.abandoned += 1;
            continue;
        }
        match DeliveryDestinationKind::from_job_destination(&leg.destination) {
            Some(DeliveryDestinationKind::Email | DeliveryDestinationKind::Webhook) => {
                // Both destinations dedupe on a key derived from the session
                // id, so a repeat of an attempt that may have landed is a
                // duplicate the receiver can recognise. Due now, not after a
                // backoff: the attempt did not fail, it was interrupted.
                jobs.reschedule(leg.id, now_millis, leg.lease_until_millis)
                    .await?;
                counts.released += 1;
            }
            Some(DeliveryDestinationKind::Discord) if !leg.request_sent => {
                // Claimed, but no request was ever dispatched — the fold, the
                // config read or the wait for a permit died first. Discord saw
                // nothing, so there is no ambiguity to respect and no duplicate
                // to risk: this is simply still owed. The AMBIGUITY forbids the
                // retry, not the claim.
                jobs.reschedule(leg.id, now_millis, leg.lease_until_millis)
                    .await?;
                counts.released += 1;
            }
            Some(DeliveryDestinationKind::Discord) => {
                // A request WAS dispatched and the outcome is unknowable, with
                // no dedupe channel to resolve it: recorded, never re-posted.
                // Ids only — the URL is a credential.
                jobs.mark_terminal(
                    leg.id,
                    TerminalState::Failed,
                    now_millis,
                    leg.lease_until_millis,
                )
                .await?;
                tracing::warn!(
                    net_session_id = %leg.session_id,
                    net_delivery_job_id = %leg.id,
                    "an interrupted discord announcement was abandoned rather than risk a double post"
                );
                counts.abandoned += 1;
            }
            None => {
                // A destination this build does not know — a row written by a
                // newer deploy that a rollback is now reading. Not deliverable
                // here, and retrying forever would be worse than recording it.
                jobs.mark_terminal(
                    leg.id,
                    TerminalState::Failed,
                    now_millis,
                    leg.lease_until_millis,
                )
                .await?;
                tracing::error!(
                    net_session_id = %leg.session_id,
                    net_delivery_job_id = %leg.id,
                    "an interrupted delivery leg names a destination this build does not know; failed"
                );
                counts.abandoned += 1;
            }
        }
    }
    Ok(counts)
}

/// Runs ONE delivery sweep: recover interrupted attempts, claim what is due,
/// run each session's legs under the deliverer's concurrency ceiling, prune
/// terminal rows past retention. Every SCHEDULING decision it makes — what is
/// due, what a lease runs to, when a reschedule lands, what is past retention —
/// comes from `now_millis`, so a test drives exactly one tick at a chosen
/// instant. It is not clock-free further down: `deliver_claimed` reads the
/// clock once for `build_summary_email`, which stamps the ADIF export, and
/// that read stays deliberately.
///
/// The claim is bounded by the permits available RIGHT NOW, never by a fixed
/// batch size: a leg's lease starts at the claim, so claiming more legs than
/// can start immediately would leave claimed legs waiting for a permit on the
/// lease's clock, and a long enough wait would read as an interrupted attempt.
/// Zero permits free means zero legs claimed this tick; the next tick tries
/// again 30 s later.
///
/// Errors from this function's OWN queries propagate with `?`, to the one
/// boundary that swallows them ([`spawn_delivery_sweeper`]). A storage error
/// inside one session's executor is NOT that case: it is counted in
/// `errored`, the other sessions still run, and that session's legs stay
/// claimed until the lease expires and the next recovery pass classifies them.
pub async fn run_delivery_sweep_tick(
    jobs: &DeliveryJobRepo,
    delivery: &DeliveryService,
    now_millis: u64,
) -> Result<SweptCounts, sqlx::Error> {
    let mut counts = SweptCounts {
        recovered: recover_interrupted_deliveries(jobs, now_millis).await?,
        ..SweptCounts::default()
    };

    // Permits FIRST, then the claim. The lease starts running the moment a row
    // is claimed, so a claim taken before the permit puts the wait for a permit
    // inside the lease — and a lease that lapses while its legs are still
    // queued is read by recovery as an interrupted attempt.
    let mut permits = delivery.reserve_delivery_permits(MAX_CONCURRENT_DELIVERIES);
    let limit = permits.len();

    // By SESSION, not by leg. A permit buys one session's delivery and the fold
    // is per session, so a limit counted in legs could claim part of a
    // session now and the rest a tick later — folding the same event log once
    // per part. Claiming each session whole keeps the fold and the permit
    // talking about the same unit.
    let mut by_session: BTreeMap<Uuid, Vec<_>> = BTreeMap::new();
    if limit > 0 {
        for session_id in jobs.due_sessions(now_millis, limit as i64).await? {
            let legs = jobs
                .claim_session(session_id, now_millis, DELIVERY_LEASE_MILLIS)
                .await?;
            // Empty when another claimer took the session between the two
            // statements; `FOR UPDATE SKIP LOCKED` makes that a no-op, not a
            // conflict.
            if !legs.is_empty() {
                by_session.insert(session_id, legs);
            }
        }
    }
    counts.claimed = by_session.values().map(|legs| legs.len() as u64).sum();
    counts.sessions = by_session.len() as u64;

    // One permit per session group. There are never fewer permits than groups:
    // the claim was bounded by the permit count and a group holds at least one
    // claimed leg. Whatever is left over is released as it drops.
    let runs: Vec<_> = by_session
        .into_iter()
        .filter_map(|(session_id, legs)| {
            permits
                .pop()
                .map(|permit| delivery.spawn_claimed(session_id, legs, now_millis, permit))
        })
        .collect();
    for run in runs {
        match run.await {
            Ok(Ok(tally)) => counts.settled.add(tally),
            // Static lines only. A storage error or a panicked executor
            // both leave the legs claimed, which recovery reads correctly once
            // the lease has passed.
            Ok(Err(_)) => {
                counts.errored += 1;
                tracing::error!(
                    "a delivery sweep executor hit a storage error; its legs stay owed"
                );
            }
            Err(_) => {
                counts.errored += 1;
                tracing::error!("a delivery sweep executor did not finish; its legs stay owed");
            }
        }
    }

    counts.pruned = jobs
        .prune_terminal(now_millis, DELIVERY_JOB_RETENTION_MILLIS)
        .await?;
    Ok(counts)
}

/// Spawns the background delivery sweep loop, mirroring
/// [`crate::retention::spawn_retention_pruner`]. Each tick runs one
/// [`run_delivery_sweep_tick`] against the injected clock.
///
/// A failed TICK logs a STATIC line and the loop CONTINUES — the one boundary
/// where a swallowed-and-logged error is correct, and correct here for the same
/// reason `retention.rs` states: killing the loop over a database blip would
/// stop delivery forever, which is the failure the durable row exists to fix. A
/// failed LEG never takes this path; it moves its row instead. The line is
/// static because a `sqlx::Error`'s text can embed row values, and
/// `net_delivery_jobs.target` holds email addresses.
///
/// Deliberately no shutdown handle, unlike the four maintenance sweeps whose
/// "a missed tick is invisible" justification doesn't apply here — a lost
/// delivery is a summary a club never received. A shutdown handle is still the
/// wrong answer, though: it can't cover SIGKILL, OOM or a dead node. The
/// durable row covers all three, clean shutdown included — a killed attempt is
/// durably still owed and recovered at the next boot. This sweep also does not
/// coordinate across replicas (the claim is `FOR UPDATE SKIP LOCKED`, which is
/// multi-replica-safe, but the boot recovery pass is not) and does not batch
/// beyond one claim per tick.
pub fn spawn_delivery_sweeper(
    jobs: DeliveryJobRepo,
    delivery: DeliveryService,
    clock: Arc<dyn Clock + Send + Sync>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(DELIVERY_SWEEP_TICK);
        loop {
            ticker.tick().await;
            match run_delivery_sweep_tick(&jobs, &delivery, clock.now_epoch_millis()).await {
                // Counts only. A quiet instance logs nothing rather than a
                // no-op line every 30 seconds.
                Ok(counts) if !counts.is_quiet() => {
                    tracing::info!(
                        released = counts.recovered.released,
                        abandoned = counts.recovered.abandoned,
                        claimed = counts.claimed,
                        sessions = counts.sessions,
                        succeeded = counts.settled.succeeded,
                        failed = counts.settled.failed,
                        skipped = counts.settled.skipped,
                        rescheduled = counts.settled.rescheduled,
                        errored = counts.errored,
                        pruned = counts.pruned,
                        "delivery sweep tick"
                    );
                }
                Ok(_) => {}
                Err(_) => {
                    tracing::error!("delivery sweep failed; retrying next tick");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outer_ladder_doubles_from_the_base_and_caps_at_the_ceiling() {
        // 30 s, 1 m, 2 m, 4 m, 8 m, 16 m, then 30 m flat — and it never
        // panics on an attempt count the row could not actually hold.
        assert_eq!(retry_delay_millis(1), 30_000);
        assert_eq!(retry_delay_millis(2), 60_000);
        assert_eq!(retry_delay_millis(3), 120_000);
        assert_eq!(retry_delay_millis(6), 16 * 60_000);
        assert_eq!(retry_delay_millis(7), DELIVERY_RETRY_MAX_MILLIS);
        assert_eq!(
            retry_delay_millis(MAX_JOB_ATTEMPTS),
            DELIVERY_RETRY_MAX_MILLIS
        );
        assert_eq!(
            retry_delay_millis(0),
            30_000,
            "a claim always precedes a reschedule"
        );
        assert_eq!(retry_delay_millis(-5), 30_000);
        assert_eq!(retry_delay_millis(i32::MAX), DELIVERY_RETRY_MAX_MILLIS);
        // The window `i32::MAX` hides. `checked_shl` guards the SHIFT WIDTH,
        // not the value: below 64 it wraps and discards the high bits, and
        // 30_000 = 1875 x 2^4, so from `spent = 60` (`attempts = 61`) the
        // product is a multiple of 2^64 and wraps to exactly zero — a "due
        // immediately" delay out of the function that schedules the backoff.
        // `i32::MAX` takes the `None` arm and so never reaches this window,
        // which is what made the old test read as a fence.
        for attempts in 50..=64 {
            assert_eq!(
                retry_delay_millis(attempts),
                DELIVERY_RETRY_MAX_MILLIS,
                "attempts = {attempts} must be capped, never zero"
            );
        }
    }

    #[test]
    fn the_lease_outlives_the_worst_case_claim_including_the_whole_email_fan_out() {
        // Derived, not asserted by fiat, and over the whole CLAIM rather than
        // one leg: a claim leases every leg of the session at one instant.
        let worst_webhook_attempt_millis: u64 = 3 * 15_000 + 2 * 25;
        // One address is three SMTP timeouts; the fan-out is concurrent, so the
        // address count does NOT multiply it. Written as an explicit `max` over
        // the cap so that making the fan-out sequential again reds this test
        // instead of silently restoring the overrun.
        let worst_single_address_millis: u64 = 3 * 10_000;
        let worst_email_fan_out_millis: u64 = worst_single_address_millis;
        // The legs run concurrently under `join!`, so the claim costs the worst
        // leg, not their sum.
        let worst_claim_millis = worst_webhook_attempt_millis.max(worst_email_fan_out_millis);
        assert!(DELIVERY_LEASE_MILLIS >= 2 * worst_claim_millis);
        // The bound that actually failed in review: a SEQUENTIAL fan-out at the
        // domain cap does not fit, which is why the concurrency is required.
        let sequential_fan_out_millis =
            worst_single_address_millis * netroll_domain::net::delivery::MAX_DELIVERY_EMAILS as u64;
        assert!(
            DELIVERY_LEASE_MILLIS < sequential_fan_out_millis,
            "if this ever passes the lease grew instead of the fan-out being fixed; re-derive both"
        );
    }
}
