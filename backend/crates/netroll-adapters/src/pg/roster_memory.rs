// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The LOCAL half of check-in prefill: the name and location a callsign was
//! last logged with on THIS definition, as one indexed PK read. Upserted
//! INSIDE the check-in append's transaction, so it cannot diverge, and always
//! rebuildable from `session_events`. Keyed per definition, so a callsign
//! remembered on one never surfaces for another — a privacy boundary.

use sqlx::PgPool;
use uuid::Uuid;

use super::utc_from_millis;

/// The remembered station identity for a `(definition_id, callsign)` — name and
/// location ONLY. Both `Option` because a callsign may have been
/// logged with only one, or neither, of the two. No operational field is ever
/// carried here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RememberedStation {
    /// The remembered operator name, or `None` if never supplied.
    pub name: Option<String>,
    /// The remembered free-text location, or `None` if never supplied.
    pub location: Option<String>,
}

/// Whether [`upsert_roster_memory_in_tx`] MERGE-preserves a previously
/// remembered non-null `name`/`location` when the incoming value is absent, or
/// REPLACES unconditionally.
///
/// [`PreserveOnNull`](MergePolicy::PreserveOnNull) is the `checkin.added`
/// policy: a later callsign-only add must never erase a name/location this
/// net already remembered — the operator simply didn't retype it.
/// [`ReplaceOnNull`](MergePolicy::ReplaceOnNull) is the `checkin.updated`
/// policy: an edit carries the FULL post-edit field set with last-write-wins
/// REPLACE semantics on the roster entry itself (mirrors the fold's own
/// `checkin.updated` REPLACE, `fold.rs`) — so if staff deliberately blank a
/// Name/Location field to correct a mistaken entry, the memory must reflect
/// that cleared value too, not silently keep the stale one forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergePolicy {
    /// `COALESCE(EXCLUDED.<col>, net_definition_roster.<col>)` — an absent
    /// incoming value never erases a remembered one.
    PreserveOnNull,
    /// `EXCLUDED.<col>` unconditionally — the incoming value, including a
    /// deliberate `NULL`, always wins.
    ReplaceOnNull,
}

/// Upserts the roster-memory row for `(definition_id, callsign)` on a BORROWED
/// connection — the in-transaction core hooked into the check-in append
/// transactions so memory commits atomically with the event.
///
/// Last-write-wins on recency (`last_seen_at` = the committing event's `at`);
/// `merge_policy` selects whether an absent `name`/`location` preserves the
/// prior remembered value (the ADD path) or replaces it outright,
/// including to `NULL` (the EDIT path — a deliberate clear must stick).
/// `check_in_count` bumps by one either way. `callsign` is the already-
/// normalized base-call string (the `Callsign` newtype form) so the stored key
/// matches the lookup's normalized key.
pub(crate) async fn upsert_roster_memory_in_tx(
    conn: &mut sqlx::PgConnection,
    definition_id: Uuid,
    callsign: &str,
    name: Option<&str>,
    location: Option<&str>,
    at_millis: u64,
    merge_policy: MergePolicy,
) -> Result<(), sqlx::Error> {
    let at = utc_from_millis(at_millis);
    match merge_policy {
        MergePolicy::PreserveOnNull => {
            sqlx::query!(
                "INSERT INTO net_definition_roster
                    (definition_id, callsign, name, location, last_seen_at, check_in_count)
                 VALUES ($1, $2, $3, $4, $5, 1)
                 ON CONFLICT (definition_id, callsign) DO UPDATE SET
                    name = COALESCE(EXCLUDED.name, net_definition_roster.name),
                    location = COALESCE(EXCLUDED.location, net_definition_roster.location),
                    last_seen_at = EXCLUDED.last_seen_at,
                    check_in_count = net_definition_roster.check_in_count + 1",
                definition_id,
                callsign,
                name,
                location,
                at,
            )
            .execute(&mut *conn)
            .await?;
        }
        MergePolicy::ReplaceOnNull => {
            sqlx::query!(
                "INSERT INTO net_definition_roster
                    (definition_id, callsign, name, location, last_seen_at, check_in_count)
                 VALUES ($1, $2, $3, $4, $5, 1)
                 ON CONFLICT (definition_id, callsign) DO UPDATE SET
                    name = EXCLUDED.name,
                    location = EXCLUDED.location,
                    last_seen_at = EXCLUDED.last_seen_at,
                    check_in_count = net_definition_roster.check_in_count + 1",
                definition_id,
                callsign,
                name,
                location,
                at,
            )
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

/// Postgres repository for the roster-memory projection.
#[derive(Clone)]
pub struct RosterMemoryRepo {
    pool: PgPool,
}

impl RosterMemoryRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Upserts one roster-memory row in its own transaction — the thin
    /// `begin → core → commit` wrapper over [`upsert_roster_memory_in_tx`], used
    /// where an upsert is NOT already inside a check-in transaction. The
    /// production check-in paths call the in-tx core directly so the upsert
    /// commits atomically with the event append.
    ///
    /// Uses [`MergePolicy::PreserveOnNull`] (the ADD-path policy) — the policy
    /// this test/utility convenience method has always exercised. Production's
    /// EDIT path uses [`MergePolicy::ReplaceOnNull`] directly via
    /// [`upsert_roster_memory_in_tx`] (see `net_sessions::apply_guarded_check_in_edit`).
    pub async fn upsert(
        &self,
        definition_id: Uuid,
        callsign: &str,
        name: Option<&str>,
        location: Option<&str>,
        at_millis: u64,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        upsert_roster_memory_in_tx(
            &mut tx,
            definition_id,
            callsign,
            name,
            location,
            at_millis,
            MergePolicy::PreserveOnNull,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Looks up the remembered name/location for `(definition_id, callsign)` — a
    /// single indexed PK read. A miss, or a callsign remembered
    /// only on a DIFFERENT definition, is `None` (definition-scoped).
    pub async fn lookup(
        &self,
        definition_id: Uuid,
        callsign: &str,
    ) -> Result<Option<RememberedStation>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT name, location FROM net_definition_roster
             WHERE definition_id = $1 AND callsign = $2",
            definition_id,
            callsign,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| RememberedStation {
            name: r.name,
            location: r.location,
        }))
    }
}
