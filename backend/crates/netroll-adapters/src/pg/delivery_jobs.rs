// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! One row per destination LEG of a session close, planned INSIDE the close
//! transaction. ALL SQL and NO policy: the backoff ladder and the
//! transient/permanent split belong to the app layer. `pending` with a non-NULL
//! `claimed_until` is the ONE representation of "in flight", and `attempts`
//! increments at CLAIM, so a killed attempt still burns one.

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{millis_from_utc, utc_from_millis};

/// The `destination` value of an email leg; `target` carries the address.
pub const DESTINATION_EMAIL: &str = "email";
/// The `destination` value of the signed-webhook leg; `target` is `''`.
pub const DESTINATION_WEBHOOK: &str = "webhook";
/// The `destination` value of the Discord-announcement leg; `target` is `''`.
pub const DESTINATION_DISCORD: &str = "discord";

/// The `state` value of a leg still owed.
const STATE_PENDING: &str = "pending";

/// The three ways a leg leaves `pending`. There is no `Pending` member on
/// purpose: a caller cannot hand [`DeliveryJobRepo::mark_terminal`] a state that
/// is not terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalState {
    /// The leg was sent: a 2xx, or the mailer returned `Ok`.
    Succeeded,
    /// The leg will never be sent: a permanent refusal, an exhausted attempt
    /// budget, or an interrupted attempt whose outcome cannot be known and
    /// must not be repeated.
    Failed,
    /// The leg was deliberately not sent: its target was cleared between the
    /// plan and the send, or its session's log can never be folded.
    Skipped,
}

impl TerminalState {
    /// The stored `state` column value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// One claimed (or interrupted) leg, as the executor needs it. Never carries a
/// state or a timestamp: a row this struct describes is `pending` by
/// construction of the queries that return it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryJob {
    /// The leg's own UUIDv7 id.
    pub id: Uuid,
    /// The closed session whose summary this leg delivers.
    pub session_id: Uuid,
    /// One of the `DESTINATION_*` values.
    pub destination: String,
    /// The email address for an email leg; `''` for the other two.
    pub target: String,
    /// How many claims this leg has had, INCLUDING the one that returned it.
    pub attempts: i32,
    /// Whether a request was actually DISPATCHED during the claim in flight.
    ///
    /// `attempts` is burned at claim, so it cannot answer this: everything
    /// between the claim and `egress.send` can fail without the destination
    /// seeing anything. Every claim resets the underlying column, so this
    /// always describes the current attempt and never an earlier one. It is
    /// what lets recovery separate a genuinely ambiguous in-flight send from a
    /// claim that never left the process.
    pub request_sent: bool,
    /// The lease this row was carrying when it was read — the claim's identity.
    ///
    /// Passed back to every write that settles the leg so a run whose send
    /// outlived its lease cannot move a row a NEWER claim already owns. The
    /// row's `claimed_until` is unique per claim in practice because each claim
    /// stamps `now + lease` at its own instant, and a claim can only happen
    /// once the previous lease was released.
    pub lease_until_millis: u64,
}

/// Plans the legs for a just-closed session INSIDE the caller's transaction.
///
/// Reads `net_delivery_configs` through the session's definition in the same
/// transaction and writes one leg per armed destination — one per address in
/// `delivery_emails`, one for `webhook_url` if present, one for
/// `discord_webhook_url` if present — and NOTHING when the config is absent or
/// arms no destination, which is the delivery-off no-op preserved
/// as "zero legs planned". `ON CONFLICT DO NOTHING` on the leg's identity makes
/// the plan idempotent, so a re-close that somehow reached it cannot duplicate
/// a leg. Returns the number of legs planned.
///
/// Lives here and is CALLED from `net_sessions::close` so the SQL stays with its
/// table rather than a second table's DDL knowledge migrating into the sessions
/// repo. `pub(super)` because the only legitimate caller is that sibling: an
/// enqueue from anywhere outside the close transaction reintroduces the loss
/// window this table exists to remove.
pub(super) async fn plan_in_tx(
    conn: &mut PgConnection,
    session_id: Uuid,
    closed_at_millis: u64,
) -> Result<u64, sqlx::Error> {
    let config = sqlx::query!(
        r#"SELECT c.delivery_emails, c.webhook_url, c.discord_webhook_url
           FROM net_sessions s
           JOIN net_delivery_configs c ON c.definition_id = s.definition_id
           WHERE s.id = $1"#,
        session_id,
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(config) = config else {
        return Ok(0);
    };

    let mut legs: Vec<(&str, &str)> = config
        .delivery_emails
        .iter()
        .map(|address| (DESTINATION_EMAIL, address.as_str()))
        .collect();
    if config.webhook_url.is_some() {
        legs.push((DESTINATION_WEBHOOK, ""));
    }
    if config.discord_webhook_url.is_some() {
        legs.push((DESTINATION_DISCORD, ""));
    }

    // The first attempt is due the instant the session closed: nothing waits on
    // a tick for the common case, because the close path claims and runs these
    // legs itself right after the commit.
    let due = utc_from_millis(closed_at_millis);
    let mut planned = 0u64;
    for (destination, target) in legs {
        let result = sqlx::query!(
            r#"INSERT INTO net_delivery_jobs
                   (id, session_id, destination, target, next_attempt_at, created_at)
               VALUES ($1, $2, $3, $4, $5, $5)
               ON CONFLICT (session_id, destination, target) DO NOTHING"#,
            Uuid::now_v7(),
            session_id,
            destination,
            target,
            due,
        )
        .execute(&mut *conn)
        .await?;
        planned += result.rows_affected();
    }
    Ok(planned)
}

/// Postgres repository for `net_delivery_jobs`.
#[derive(Clone)]
pub struct DeliveryJobRepo {
    pool: PgPool,
}

impl DeliveryJobRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The sessions with at least one due, unclaimed leg, soonest first.
    ///
    /// The sweeper claims by SESSION, not by leg: a permit buys one session's
    /// delivery and the fold is per session, so a limit counted in legs
    /// could take part of a session now and the rest a tick later, folding the
    /// same event log twice. Ordered by the session's earliest due leg so a
    /// backlog drains oldest-first.
    pub async fn due_sessions(
        &self,
        now_millis: u64,
        limit: i64,
    ) -> Result<Vec<Uuid>, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let rows = sqlx::query!(
            r#"SELECT session_id, min(next_attempt_at) AS "due!"
                 FROM net_delivery_jobs
                WHERE state = 'pending'
                  AND next_attempt_at <= $1
                  AND claimed_until IS NULL
                GROUP BY session_id
                ORDER BY "due!", session_id
                LIMIT $2"#,
            now,
            limit,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.session_id).collect())
    }

    /// Claims every due, unclaimed leg of ONE session — the close path's own
    /// claim, taken right after the commit so the common case never waits on a
    /// sweep tick. Same lease, same attempt burn, same `SKIP LOCKED` posture as
    /// [`Self::due_sessions`]; no limit, because a session has at most a handful
    /// of legs (`delivery_emails` is domain-capped, plus at most one webhook and
    /// one Discord).
    pub async fn claim_session(
        &self,
        session_id: Uuid,
        now_millis: u64,
        lease_millis: u64,
    ) -> Result<Vec<DeliveryJob>, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let lease_until = utc_from_millis(now_millis.saturating_add(lease_millis));
        let rows = sqlx::query!(
            r#"UPDATE net_delivery_jobs
                  SET attempts = attempts + 1,
                      claimed_until = $3,
                      request_sent_at = NULL
                WHERE id IN (
                      SELECT id FROM net_delivery_jobs
                       WHERE session_id = $1
                         AND state = 'pending'
                         AND next_attempt_at <= $2
                         AND claimed_until IS NULL
                       ORDER BY destination, target
                       FOR UPDATE SKIP LOCKED)
            RETURNING id, session_id, destination, target, attempts, request_sent_at,
                      claimed_until"#,
            session_id,
            now,
            lease_until,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| DeliveryJob {
                id: r.id,
                session_id: r.session_id,
                destination: r.destination,
                target: r.target,
                attempts: r.attempts,
                request_sent: r.request_sent_at.is_some(),
                lease_until_millis: r.claimed_until.map(millis_from_utc).unwrap_or_default(),
            })
            .collect())
    }

    /// The legs whose attempt died in flight: still `pending`, but carrying a
    /// lease that has passed. Read-only — deciding what to do with each is the
    /// Every `pending` leg that is currently CLAIMED, whatever its lease says.
    ///
    /// The boot pass's selection. At boot no attempt of this process can be in
    /// flight, so a claim still inside its lease is a DEAD process's claim, not
    /// a live one — the expiry filter that is right for a running tick makes
    /// the boot pass a no-op for exactly the crash it exists to handle, which
    /// is what shipped.
    pub async fn claimed(&self, limit: i64) -> Result<Vec<DeliveryJob>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT id, session_id, destination, target, attempts, request_sent_at,
                      claimed_until
                 FROM net_delivery_jobs
                WHERE state = 'pending'
                  AND claimed_until IS NOT NULL
                ORDER BY claimed_until, session_id
                LIMIT $1"#,
            limit,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| DeliveryJob {
                id: r.id,
                session_id: r.session_id,
                destination: r.destination,
                target: r.target,
                attempts: r.attempts,
                request_sent: r.request_sent_at.is_some(),
                lease_until_millis: r.claimed_until.map(millis_from_utc).unwrap_or_default(),
            })
            .collect())
    }

    /// app layer's per-destination policy.
    pub async fn interrupted(
        &self,
        now_millis: u64,
        limit: i64,
    ) -> Result<Vec<DeliveryJob>, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let rows = sqlx::query!(
            r#"SELECT id, session_id, destination, target, attempts, request_sent_at,
                       claimed_until
                 FROM net_delivery_jobs
                WHERE state = 'pending'
                  AND claimed_until IS NOT NULL
                  AND claimed_until <= $1
                ORDER BY claimed_until, session_id
                LIMIT $2"#,
            now,
            limit,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| DeliveryJob {
                id: r.id,
                session_id: r.session_id,
                destination: r.destination,
                target: r.target,
                attempts: r.attempts,
                request_sent: r.request_sent_at.is_some(),
                lease_until_millis: r.claimed_until.map(millis_from_utc).unwrap_or_default(),
            })
            .collect())
    }

    /// Moves one still-`pending` leg to a terminal state, stamping
    /// `completed_at` and releasing its lease. A leg already terminal is left
    /// alone (the guard is `state = 'pending'`), so two racing settlements
    /// cannot overwrite each other's verdict.
    /// Records that a request for this leg has been DISPATCHED to its
    /// destination during the claim in flight.
    ///
    /// Called immediately before the send, never after, because the whole point
    /// is to be durable across a process that dies mid-request. The window this
    /// leaves — written, then the process dies before the request actually
    /// leaves the socket — is deliberately biased towards "possibly sent": for
    /// a destination with no dedupe channel a false "sent" costs one lost
    /// announcement, while a false "not sent" costs a double post, which the
    /// epic calls strictly worse.
    ///
    /// Guarded on `state = 'pending'` like the other settle-time writes, so a
    /// leg some other pass already terminated is not resurrected.
    pub async fn mark_request_sent(
        &self,
        id: Uuid,
        now_millis: u64,
        lease_until_millis: u64,
    ) -> Result<(), sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let lease = utc_from_millis(lease_until_millis);
        sqlx::query!(
            r#"UPDATE net_delivery_jobs
                  SET request_sent_at = $2
                WHERE id = $1 AND state = 'pending' AND claimed_until = $3"#,
            id,
            now,
            lease,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Moves a `pending` leg to a terminal state, but only while the caller
    /// still holds the lease it claimed with; returns the rows changed, so `0`
    /// means the lease was lost and the outcome must not be recorded.
    pub async fn mark_terminal(
        &self,
        id: Uuid,
        state: TerminalState,
        now_millis: u64,
        lease_until_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let lease = utc_from_millis(lease_until_millis);
        let result = sqlx::query!(
            r#"UPDATE net_delivery_jobs
                  SET state = $2, completed_at = $3, claimed_until = NULL
                WHERE id = $1 AND state = 'pending' AND claimed_until = $4"#,
            id,
            state.as_str(),
            now,
            lease,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Releases a `pending` leg's lease and schedules its next attempt. Used
    /// both for the transient-failure backoff and for recovering an interrupted
    /// attempt whose destination may be retried.
    pub async fn reschedule(
        &self,
        id: Uuid,
        next_attempt_at_millis: u64,
        lease_until_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let next = utc_from_millis(next_attempt_at_millis);
        let lease = utc_from_millis(lease_until_millis);
        let result = sqlx::query!(
            r#"UPDATE net_delivery_jobs
                  SET claimed_until = NULL, next_attempt_at = $2
                WHERE id = $1 AND state = 'pending' AND claimed_until = $3"#,
            id,
            next,
            lease,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Marks EVERY still-`pending` leg of a session `skipped` — the whole
    /// session, not the leg that happened to be claimed, because the reason
    /// (a log that can never be folded) is a property of the session. Returns
    /// how many legs moved.
    pub async fn skip_session(
        &self,
        session_id: Uuid,
        now_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let result = sqlx::query!(
            r#"UPDATE net_delivery_jobs
                  SET state = $2, completed_at = $3, claimed_until = NULL
                WHERE session_id = $1 AND state = 'pending'"#,
            session_id,
            TerminalState::Skipped.as_str(),
            now,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Deletes terminal rows whose `completed_at` is older than the retention
    /// window. Idempotent: a second run over the same cutoff removes nothing.
    /// `pending` rows are never touched, however old — a debt is a debt until
    /// it is settled.
    pub async fn prune_terminal(
        &self,
        now_millis: u64,
        retention_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let cutoff = utc_from_millis(now_millis.saturating_sub(retention_millis));
        let result = sqlx::query!(
            r#"DELETE FROM net_delivery_jobs
                WHERE state <> $1 AND completed_at IS NOT NULL AND completed_at <= $2"#,
            STATE_PENDING,
            cutoff,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
