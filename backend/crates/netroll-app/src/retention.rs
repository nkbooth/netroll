// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The background retention sweep for the three credential-artifact tables.
//!
//! `ON DELETE CASCADE` reaches neither leak, so this sweep is time-based and
//! account-independent. Nothing FK-references these tables and it selects only
//! rows past the window, so its locks cannot collide with the auth hot path.

use std::sync::Arc;
use std::time::Duration;

use netroll_adapters::pg::email_changes::EmailChangeRepo;
use netroll_adapters::pg::magic_links::MagicLinkRepo;
use netroll_adapters::pg::sessions::SessionRepo;
use netroll_domain::ports::Clock;

/// How often the retention sweep ticks. An hour, matching the occurrence
/// materializer rather than the finalizer's 60s or the presence monitor's 30s:
/// those two race a user-visible deadline (a 15-minute undelete window, a ~90s
/// stall threshold), while nothing observes a row between the instant it dies
/// and the instant it is pruned. Against a 30-day window an hour of lag is
/// noise, and `tokio::time::interval` completes its first tick immediately, so
/// the first sweep runs at boot rather than an hour in.
pub const RETENTION_TICK: Duration = Duration::from_secs(60 * 60);

/// How many rows one sweep removed, per table. Reported so the caller can log
/// counts (never row values — all three tables hold email addresses) and
/// so a test can assert the sweep's own account of its work alongside the
/// database state it left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrunedCounts {
    /// Expired magic-link tokens removed.
    pub magic_link_tokens: u64,
    /// Expired-or-revoked sessions removed.
    pub sessions: u64,
    /// Expired email-change tokens removed.
    pub email_change_tokens: u64,
}

/// Runs ONE retention sweep across all three tables against the same cutoff.
///
/// Separated from the spawn loop and parameterized on `now_millis` +
/// `retention_millis` so a test drives exactly one sweep with a fake clock —
/// no sleeps, no real hour elapsing, following
/// [`crate::presence_monitor::run_presence_monitor_tick`]. Pure in its
/// arguments: it never reads a clock itself, so "past the window" is set by the
/// caller rather than waited for.
///
/// Errors propagate with `?`. Sweeping is idempotent — a `DELETE` over a
/// cutoff removes nothing the second time — so a partially-completed tick is
/// simply redone by the next one.
pub async fn run_retention_prune_tick(
    magic_links: &MagicLinkRepo,
    sessions: &SessionRepo,
    email_changes: &EmailChangeRepo,
    now_millis: u64,
    retention_millis: u64,
) -> Result<PrunedCounts, sqlx::Error> {
    Ok(PrunedCounts {
        magic_link_tokens: magic_links
            .prune_expired(now_millis, retention_millis)
            .await?,
        sessions: sessions.prune_dead(now_millis, retention_millis).await?,
        email_change_tokens: email_changes
            .prune_expired(now_millis, retention_millis)
            .await?,
    })
}

/// Spawns the background retention loop, mirroring
/// [`crate::finalizer::spawn_deletion_finalizer`]. Each tick runs one
/// idempotent [`run_retention_prune_tick`] against the injected clock.
///
/// A transient DB error is logged and the loop CONTINUES. This is the single
/// boundary where an error stops — the three repo methods and the tick function
/// all propagate with `?` — and it is correct only here: killing the loop over a
/// database blip would silently stop pruning forever, which is the exact failure
/// this sweep exists to fix. The line is STATIC, never the `sqlx::Error`, whose
/// text can embed row values; all three tables hold email addresses.
///
/// Nothing here waits for the sweep to finish at shutdown and nothing retries
/// within a tick. A tick lost to a restart or an error is simply picked up by
/// the next one. Best-effort by design, not a durable job queue — no general
/// job runner is warranted for a single-replica deploy where a missed tick
/// self-heals. Each `DELETE` is one unbounded, unindexed-predicate statement:
/// worst case is the first-boot sweep against never-pruned tables, which a
/// mid-sweep restart rolls back whole, re-incurred every boot until one sweep
/// survives to completion.
pub fn spawn_retention_pruner(
    magic_links: MagicLinkRepo,
    sessions: SessionRepo,
    email_changes: EmailChangeRepo,
    clock: Arc<dyn Clock + Send + Sync>,
    retention_millis: u64,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(RETENTION_TICK);
        loop {
            ticker.tick().await;
            match run_retention_prune_tick(
                &magic_links,
                &sessions,
                &email_changes,
                clock.now_epoch_millis(),
                retention_millis,
            )
            .await
            {
                // Counts only — not PII. A quiet instance logs nothing rather
                // than an hourly no-op line.
                Ok(pruned) if pruned != PrunedCounts::default() => {
                    tracing::info!(
                        magic_link_tokens = pruned.magic_link_tokens,
                        sessions = pruned.sessions,
                        email_change_tokens = pruned.email_change_tokens,
                        "expired tokens and dead sessions pruned"
                    );
                }
                Ok(_) => {}
                Err(_) => {
                    tracing::error!("token retention sweep failed; retrying next tick");
                }
            }
        }
    });
}
