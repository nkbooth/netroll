// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The background finalizer for account self-deletion.
//!
//! A spawned interval loop that hard-deletes accounts whose grace window has
//! elapsed, so an abandoned pending account never lingers holding PII. Best
//! effort: a missed tick is invisible and the next tick self-heals.

use std::sync::Arc;
use std::time::Duration;

use netroll_adapters::pg::accounts::AccountRepo;
use netroll_adapters::pg::net_definitions::NetDefinitionRepo;
use netroll_domain::deletion::DELETION_GRACE_MILLIS;
use netroll_domain::ports::{AvatarStore, Clock};

use crate::avatar_cleanup::delete_stored_avatars;

/// How often the finalizer sweeps. The undelete window is 15 minutes, so a
/// lag of at most one tick before an eligible account is finalized is
/// acceptable and bounded; finalize-on-access is the immediate backstop for
/// any account that is signed into in the meantime.
const FINALIZER_TICK: Duration = Duration::from_secs(60);

/// Spawns the background sweep loop. Each tick hard-deletes every account
/// whose grace window has elapsed (judged against the injected clock);
/// child rows cascade via the migration FKs.
///
/// A transient DB error is logged and the loop CONTINUES — the next tick
/// retries and finalize-on-access covers the gap. This is the one place a
/// swallowed-and-logged error is correct: killing the loop would leave
/// finalized-eligible PII in place indefinitely, violating the data-right.
///
/// Each tick, right after finalizing accounts, the loop archives every
/// ownerless net: an account-finalize cascade can empty a net's
/// owner set, and `archive_ownerless` is the idempotent reconciliation that
/// archives the now-manageable-by-no-one net. Running it every tick is safe
/// (it archives nothing when no net is orphaned) and self-healing: even if a
/// tick crashed between the account delete and the archive, the next tick
/// reconciles.
///
/// No general job runner is coming to absorb this loop: a durable job queue
/// becomes warranted only if the deploy goes multi-replica (two replicas would
/// double-run every sweep) or a job's loss becomes user-visible, and neither
/// applies here. Outbound delivery WAS the one case where loss became
/// user-visible, which is why it got its own durable job table and sweeper —
/// see [`crate::delivery_sweeper`].
pub fn spawn_deletion_finalizer(
    accounts: AccountRepo,
    net_definitions: NetDefinitionRepo,
    clock: Arc<dyn Clock + Send + Sync>,
    avatar_store: Arc<dyn AvatarStore + Send + Sync>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(FINALIZER_TICK);
        loop {
            ticker.tick().await;
            match accounts
                .finalize_deletions_erasing(clock.now_epoch_millis(), DELETION_GRACE_MILLIS)
                .await
            {
                Ok(erased) if !erased.is_empty() => {
                    // The row is gone; the uploaded avatar must go with it —
                    // blob storage has no cascade, and an erased account's photo
                    // surviving on the volume defeats the erasure.
                    delete_stored_avatars(avatar_store.as_ref(), &erased).await;
                    tracing::info!(
                        finalized = erased.len(),
                        "pending-deletion accounts finalized"
                    );
                }
                Ok(_) => {}
                Err(_) => {
                    // Static line only — sqlx error text can embed row
                    // values such as email addresses.
                    tracing::error!("deletion finalizer sweep failed; retrying next tick");
                }
            }

            match net_definitions
                .archive_ownerless(clock.now_epoch_millis())
                .await
            {
                Ok(n) if n > 0 => {
                    tracing::info!(archived = n, "ownerless net definitions archived");
                }
                Ok(_) => {}
                Err(_) => {
                    tracing::error!("ownerless-net archive sweep failed; retrying next tick");
                }
            }
        }
    });
}
