// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The background task that materializes recurring occurrences into a horizon.
//!
//! Each tick calls the idempotent `spawn_due_occurrences`, which writes
//! `ON CONFLICT DO NOTHING` so re-running never duplicates a row. One-off
//! schedules and archived definitions are skipped.

use std::sync::Arc;
use std::time::Duration;

use netroll_adapters::pg::schedules::{MATERIALIZATION_HORIZON_MILLIS, ScheduleRepo};
use netroll_domain::ports::Clock;

/// How often the materializer sweeps. Coarser than the finalizer's 60 s: the
/// horizon is days wide, so an hourly tick keeps the rolling window fresh with
/// negligible lag while doing far less work.
const MATERIALIZER_TICK: Duration = Duration::from_secs(60 * 60);

/// Spawns the background materialization loop. Each tick materializes recurring
/// occurrences across the rolling horizon, judged against the injected clock.
///
/// A transient DB error is logged (a STATIC line — never definition
/// titles/geography) and the loop CONTINUES: the next tick reconciles,
/// and the `ON CONFLICT DO NOTHING` write makes a repeat harmless. This is the
/// finalizer's swallow-and-continue posture — killing the loop over a blip
/// would silently stop discovery look-ahead from advancing.
///
/// No general job runner is coming to absorb this loop, for the same reason as
/// [`crate::finalizer::spawn_deletion_finalizer`]: warranted only by a
/// multi-replica deploy or a user-visible job loss, and a materialization tick
/// reconciled a tick late is a horizon nobody noticed.
pub fn spawn_occurrence_materializer(schedules: ScheduleRepo, clock: Arc<dyn Clock + Send + Sync>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(MATERIALIZER_TICK);
        loop {
            ticker.tick().await;
            match schedules
                .spawn_due_occurrences(clock.now_epoch_millis(), MATERIALIZATION_HORIZON_MILLIS)
                .await
            {
                Ok(n) if n > 0 => {
                    tracing::info!(spawned = n, "recurring net occurrences materialized");
                }
                Ok(_) => {}
                Err(_) => {
                    tracing::error!("occurrence materializer sweep failed; retrying next tick");
                }
            }
        }
    });
}
