// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Net schedules and their planned occurrences. An occurrence is a PLANNED
//! POINTER with NO definition snapshot, unlike a session. The
//! `UNIQUE (definition_id, scheduled_start_at)` constraint plus `ON CONFLICT DO
//! NOTHING` is the idempotency guarantee: a spawn re-run never duplicates a row.

use chrono::{DateTime, Utc, Weekday};
use netroll_domain::net::schedule::{
    Frequency, NetOccurrence, RecurringSchedule, Schedule, occurrences_between,
};
use sqlx::PgPool;
use uuid::Uuid;

use super::{millis_from_utc, utc_from_millis};

/// Rolling look-ahead window recurring occurrences are materialized into (60
/// days). A judgment call — long enough for useful discovery look-ahead,
/// short enough to bound row volume; tune here or lift into `config.rs` if it
/// needs to be instance-tunable. `ON CONFLICT DO NOTHING` is what makes a
/// rolling, repeatedly-materialized window safe.
pub const MATERIALIZATION_HORIZON_MILLIS: u64 = 60 * 24 * 60 * 60 * 1000;

/// Postgres repository for `net_schedules` + `net_occurrences`.
#[derive(Clone)]
pub struct ScheduleRepo {
    pool: PgPool,
}

/// The `net_schedules` row shape (recurring-rule reconstruction lives in one
/// place).
struct ScheduleRow {
    kind: String,
    timezone: String,
    one_off_start_at: Option<DateTime<Utc>>,
    frequency: Option<String>,
    local_hour: Option<i16>,
    local_minute: Option<i16>,
    weekday: Option<i16>,
    day_of_month: Option<i16>,
}

/// Maps an out-of-vocabulary schedule value read from storage to a decode
/// error (only reachable if a non-Rust writer bypassed the domain).
fn decode_error(column: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("net_schedules.{column} holds an unknown value").into())
}

impl ScheduleRow {
    /// Reconstructs the typed [`Schedule`] from the stored columns, decoding
    /// enum/timezone columns through the domain vocabulary.
    fn into_schedule(self) -> Result<Schedule, sqlx::Error> {
        let timezone = self
            .timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| decode_error("timezone"))?;
        match self.kind.as_str() {
            "one-off" => {
                let start = self
                    .one_off_start_at
                    .ok_or_else(|| decode_error("one_off_start_at"))?;
                Ok(Schedule::OneOff {
                    start_at_millis: millis_from_utc(start),
                    timezone,
                })
            }
            "recurring" => {
                let frequency = self
                    .frequency
                    .as_deref()
                    .and_then(|s| Frequency::try_from(s).ok())
                    .ok_or_else(|| decode_error("frequency"))?;
                let hour = self.local_hour.ok_or_else(|| decode_error("local_hour"))? as u8;
                let minute =
                    self.local_minute
                        .ok_or_else(|| decode_error("local_minute"))? as u8;
                let weekday = match self.weekday {
                    None => None,
                    Some(n) => {
                        Some(Weekday::try_from(n as u8).map_err(|_| decode_error("weekday"))?)
                    }
                };
                let day_of_month = self.day_of_month.map(|n| n as u8);
                Ok(Schedule::Recurring(RecurringSchedule {
                    frequency,
                    timezone,
                    hour,
                    minute,
                    weekday,
                    day_of_month,
                }))
            }
            _ => Err(decode_error("kind")),
        }
    }
}

/// The column values a [`Schedule`] decomposes into for the upsert.
struct ScheduleColumns {
    kind: &'static str,
    timezone: String,
    one_off_start_at: Option<DateTime<Utc>>,
    frequency: Option<&'static str>,
    local_hour: Option<i16>,
    local_minute: Option<i16>,
    weekday: Option<i16>,
    day_of_month: Option<i16>,
}

impl ScheduleColumns {
    fn from_schedule(schedule: &Schedule) -> Self {
        match schedule {
            Schedule::OneOff {
                start_at_millis,
                timezone,
            } => Self {
                kind: "one-off",
                timezone: timezone.name().to_owned(),
                one_off_start_at: Some(utc_from_millis(*start_at_millis)),
                frequency: None,
                local_hour: None,
                local_minute: None,
                weekday: None,
                day_of_month: None,
            },
            Schedule::Recurring(r) => Self {
                kind: "recurring",
                timezone: r.timezone.name().to_owned(),
                one_off_start_at: None,
                frequency: Some(r.frequency.as_str()),
                local_hour: Some(r.hour as i16),
                local_minute: Some(r.minute as i16),
                weekday: r.weekday.map(|w| w.num_days_from_monday() as i16),
                day_of_month: r.day_of_month.map(|d| d as i16),
            },
        }
    }
}

/// The UTC instants a schedule materializes at set-time: a one-off's single
/// future instant (horizon-independent — a far-future one-off still
/// materializes immediately), or a recurring rule's occurrences across the
/// rolling horizon.
fn materialization_instants(schedule: &Schedule, now_millis: u64, horizon_millis: u64) -> Vec<u64> {
    match schedule {
        Schedule::OneOff {
            start_at_millis, ..
        } => {
            if *start_at_millis > now_millis {
                vec![*start_at_millis]
            } else {
                Vec::new()
            }
        }
        Schedule::Recurring(_) => {
            occurrences_between(schedule, now_millis, now_millis + horizon_millis)
        }
    }
}

impl ScheduleRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Upserts the 1:1 schedule row for `definition_id` and (re)materializes
    /// its FUTURE occurrences into the rolling horizon in ONE transaction.
    /// Future occurrences no longer matching the new rule are
    /// removed; matching ones are kept (no churn); new ones are inserted
    /// `ON CONFLICT DO NOTHING`. PAST occurrences (`scheduled_start_at <= now`)
    /// are left untouched — they are provenance. Re-calling with the SAME rule
    /// adds no duplicates (idempotent).
    pub async fn set_schedule(
        &self,
        definition_id: Uuid,
        schedule: &Schedule,
        now_millis: u64,
    ) -> Result<(), sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let cols = ScheduleColumns::from_schedule(schedule);
        let instants: Vec<DateTime<Utc>> =
            materialization_instants(schedule, now_millis, MATERIALIZATION_HORIZON_MILLIS)
                .into_iter()
                .map(utc_from_millis)
                .collect();

        let mut tx = self.pool.begin().await?;

        sqlx::query!(
            "INSERT INTO net_schedules
                (definition_id, kind, timezone, one_off_start_at, frequency,
                 local_hour, local_minute, weekday, day_of_month, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
             ON CONFLICT (definition_id) DO UPDATE SET
                kind = excluded.kind,
                timezone = excluded.timezone,
                one_off_start_at = excluded.one_off_start_at,
                frequency = excluded.frequency,
                local_hour = excluded.local_hour,
                local_minute = excluded.local_minute,
                weekday = excluded.weekday,
                day_of_month = excluded.day_of_month,
                updated_at = excluded.updated_at",
            definition_id,
            cols.kind,
            cols.timezone,
            cols.one_off_start_at,
            cols.frequency,
            cols.local_hour,
            cols.local_minute,
            cols.weekday,
            cols.day_of_month,
            now,
        )
        .execute(&mut *tx)
        .await?;

        // Remove FUTURE occurrences the new rule no longer produces; past rows
        // (<= now) are provenance and never rewritten (the principle). An
        // empty instant set deletes every future occurrence (correct: the rule
        // produces none in-horizon).
        sqlx::query!(
            "DELETE FROM net_occurrences
             WHERE definition_id = $1 AND scheduled_start_at > $2
               AND scheduled_start_at <> ALL($3)",
            definition_id,
            now,
            &instants,
        )
        .execute(&mut *tx)
        .await?;

        for at in &instants {
            sqlx::query!(
                "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (definition_id, scheduled_start_at) DO NOTHING",
                Uuid::now_v7(),
                definition_id,
                at,
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    /// Reads the stored 1:1 schedule rule for `definition_id`, or `None` when
    /// the net has no schedule — the read seam the edit form hydrates from.
    /// Reconstruction goes through the SAME
    /// [`ScheduleRow::into_schedule`] the spawner uses, so a rule read here can
    /// never disagree with the rule materialization sees.
    pub async fn get_schedule(&self, definition_id: Uuid) -> Result<Option<Schedule>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT kind, timezone, one_off_start_at, frequency,
                    local_hour, local_minute, weekday, day_of_month
             FROM net_schedules WHERE definition_id = $1",
            definition_id,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some(row) => Ok(Some(
                ScheduleRow {
                    kind: row.kind,
                    timezone: row.timezone,
                    one_off_start_at: row.one_off_start_at,
                    frequency: row.frequency,
                    local_hour: row.local_hour,
                    local_minute: row.local_minute,
                    weekday: row.weekday,
                    day_of_month: row.day_of_month,
                }
                .into_schedule()?,
            )),
        }
    }

    /// Clears the schedule for `definition_id`: deletes the `net_schedules` row
    /// and all FUTURE occurrences (`scheduled_start_at > now`), leaving past
    /// ones as provenance. Idempotent — clearing a schedule-less net is
    /// a no-op.
    pub async fn clear_schedule(
        &self,
        definition_id: Uuid,
        now_millis: u64,
    ) -> Result<(), sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "DELETE FROM net_schedules WHERE definition_id = $1",
            definition_id,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM net_occurrences
             WHERE definition_id = $1 AND scheduled_start_at > $2",
            definition_id,
            now,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The idempotent background-spawn: for every NON-ARCHIVED definition with
    /// a RECURRING schedule, materializes occurrences in `(now, now + horizon]`
    /// `ON CONFLICT DO NOTHING`, returning the count inserted.
    /// One-off schedules are skipped (fully materialized at set-time); archived
    /// definitions are skipped (their occurrences leave discovery).
    pub async fn spawn_due_occurrences(
        &self,
        now_millis: u64,
        horizon_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT s.definition_id, s.kind, s.timezone, s.one_off_start_at, s.frequency,
                    s.local_hour, s.local_minute, s.weekday, s.day_of_month
             FROM net_schedules s
             JOIN net_definitions d ON d.id = s.definition_id
             WHERE d.archived_at IS NULL AND s.kind = 'recurring'",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut inserted: u64 = 0;
        for row in rows {
            let definition_id = row.definition_id;
            let schedule = match (ScheduleRow {
                kind: row.kind,
                timezone: row.timezone,
                one_off_start_at: row.one_off_start_at,
                frequency: row.frequency,
                local_hour: row.local_hour,
                local_minute: row.local_minute,
                weekday: row.weekday,
                day_of_month: row.day_of_month,
            }
            .into_schedule())
            {
                Ok(schedule) => schedule,
                // A single out-of-vocabulary row (only reachable if a
                // non-Rust writer bypassed the domain) must not starve every
                // OTHER definition's materialization — skip it and keep
                // going. Adapters don't depend on `tracing` (that's the
                // app layer's job); the caller can observe this via a
                // stalled `updated_at` if it ever matters in practice.
                Err(_) => continue,
            };
            let instants = occurrences_between(&schedule, now_millis, now_millis + horizon_millis);
            for millis in instants {
                let at = utc_from_millis(millis);
                let result = sqlx::query!(
                    "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at)
                     VALUES ($1, $2, $3)
                     ON CONFLICT (definition_id, scheduled_start_at) DO NOTHING",
                    Uuid::now_v7(),
                    definition_id,
                    at,
                )
                .execute(&self.pool)
                .await?;
                inserted += result.rows_affected();
            }
        }
        Ok(inserted)
    }

    /// The minimal seam the discovery read consumes: upcoming
    /// occurrences of LISTED, non-archived definitions with
    /// `scheduled_start_at > now`, ascending, capped at `limit`. An Unlisted or
    /// archived net's occurrences never appear (the no-leak shape).
    pub async fn list_upcoming_occurrences(
        &self,
        now_millis: u64,
        limit: i64,
    ) -> Result<Vec<NetOccurrence>, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let rows = sqlx::query!(
            "SELECT o.id, o.definition_id, o.scheduled_start_at
             FROM net_occurrences o
             JOIN net_definitions d ON d.id = o.definition_id
             WHERE d.visibility = 'listed' AND d.archived_at IS NULL
               AND o.scheduled_start_at > $1
             ORDER BY o.scheduled_start_at ASC
             LIMIT $2",
            now,
            limit,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| NetOccurrence {
                id: r.id,
                definition_id: r.definition_id,
                scheduled_start_at_millis: millis_from_utc(r.scheduled_start_at),
            })
            .collect())
    }

    /// Upcoming occurrences for a SINGLE definition (the owner's occurrences
    /// endpoint), ascending, `scheduled_start_at > now`.
    pub async fn upcoming_for_definition(
        &self,
        definition_id: Uuid,
        now_millis: u64,
    ) -> Result<Vec<NetOccurrence>, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let rows = sqlx::query!(
            "SELECT id, definition_id, scheduled_start_at
             FROM net_occurrences
             WHERE definition_id = $1 AND scheduled_start_at > $2
             ORDER BY scheduled_start_at ASC",
            definition_id,
            now,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| NetOccurrence {
                id: r.id,
                definition_id: r.definition_id,
                scheduled_start_at_millis: millis_from_utc(r.scheduled_start_at),
            })
            .collect())
    }
}
