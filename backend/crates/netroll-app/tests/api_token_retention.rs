// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Tick-level test for the retention pruner: drives `run_retention_prune_tick`
//! directly with a fake clock against a real containerized Postgres.
//!
//! `schema_token_retention.rs` covers the three prune predicates; this covers
//! the composition. Time is an INPUT, so there are no sleeps and no timing flake.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use netroll_adapters::pg::email_changes::EmailChangeRepo;
use netroll_adapters::pg::magic_links::MagicLinkRepo;
use netroll_adapters::pg::sessions::SessionRepo;
use netroll_app::retention::run_retention_prune_tick;
use netroll_domain::ports::Clock;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use uuid::Uuid;

/// Wall clock the test owns outright — the sweep's cutoff is derived from it,
/// so "past the window" is set rather than waited for.
#[derive(Clone)]
struct FakeClock {
    millis: Arc<AtomicU64>,
}

impl FakeClock {
    fn new(start: u64) -> Self {
        Self {
            millis: Arc::new(AtomicU64::new(start)),
        }
    }
}

impl Clock for FakeClock {
    fn now_epoch_millis(&self) -> u64 {
        self.millis.load(Ordering::SeqCst)
    }
}

/// The container must stay alive as long as the pool, so both are returned.
async fn migrated_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let container = Postgres::default()
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("resolve container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("resolve mapped postgres port");

    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@{host}:{port}/postgres"
        ))
        .await
        .expect("connect to containerized postgres");

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations against fresh database");

    (container, pool)
}

const NOW_MILLIS: u64 = 1_800_000_000_000;
const RETENTION_MILLIS: u64 = 30 * 24 * 60 * 60 * 1_000;
const DAY_MILLIS: u64 = 24 * 60 * 60 * 1_000;

fn at(millis: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis as i64).expect("test instant in chrono range")
}

/// sqlx 0.9 refuses non-`'static` SQL strings, so each count is a literal.
async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .expect("count rows")
}

const COUNT_MAGIC_LINKS: &str = "SELECT count(*) FROM magic_link_tokens";
const COUNT_SESSIONS: &str = "SELECT count(*) FROM sessions";
const COUNT_EMAIL_CHANGES: &str = "SELECT count(*) FROM email_change_tokens";

/// `tokio::time::pause` is deliberately not used anywhere in this file: the
/// assertions are about database state after a cutoff comparison, and the
/// cutoff is an argument rather than an elapsed duration.
#[tokio::test]
async fn one_tick_prunes_every_table_and_reports_what_it_removed() {
    let (_container, pool) = migrated_pool().await;
    let clock = FakeClock::new(NOW_MILLIS);
    let cutoff = NOW_MILLIS - RETENTION_MILLIS;

    let account_id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, 'op@example.com')")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("seed account");

    // An orphaned (email-keyed, uncascaded) magic link plus a live one.
    for (tag, expires_at) in [
        ("ml-stale", cutoff - DAY_MILLIS),
        ("ml-live", NOW_MILLIS + 900_000),
    ] {
        sqlx::query(
            "INSERT INTO magic_link_tokens (id, email, token_hash, expires_at)
             VALUES ($1, 'op@example.com', $2, $3)",
        )
        .bind(Uuid::now_v7())
        .bind(tag.as_bytes())
        .bind(at(expires_at))
        .execute(&pool)
        .await
        .expect("seed magic-link token");
    }

    // A long-revoked session (absolute cap still far future) plus a live one.
    sqlx::query(
        "INSERT INTO sessions (id, account_id, token_hash, last_seen_at, absolute_expires_at, revoked_at)
         VALUES ($1, $2, 'se-revoked', $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(at(cutoff - DAY_MILLIS))
    .bind(at(NOW_MILLIS + 30 * DAY_MILLIS))
    .bind(at(cutoff - DAY_MILLIS))
    .execute(&pool)
    .await
    .expect("seed revoked session");
    sqlx::query(
        "INSERT INTO sessions (id, account_id, token_hash, last_seen_at, absolute_expires_at)
         VALUES ($1, $2, 'se-live', $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(at(NOW_MILLIS))
    .bind(at(NOW_MILLIS + 30 * DAY_MILLIS))
    .execute(&pool)
    .await
    .expect("seed live session");

    for (tag, expires_at) in [
        ("ec-stale", cutoff - DAY_MILLIS),
        ("ec-live", NOW_MILLIS + 900_000),
    ] {
        sqlx::query(
            "INSERT INTO email_change_tokens (id, account_id, new_email, token_hash, expires_at)
             VALUES ($1, $2, 'next@example.com', $3, $4)",
        )
        .bind(Uuid::now_v7())
        .bind(account_id)
        .bind(tag.as_bytes())
        .bind(at(expires_at))
        .execute(&pool)
        .await
        .expect("seed email-change token");
    }

    let magic_links = MagicLinkRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool.clone());

    let pruned = run_retention_prune_tick(
        &magic_links,
        &sessions,
        &email_changes,
        clock.now_epoch_millis(),
        RETENTION_MILLIS,
    )
    .await
    .expect("one retention sweep");

    assert_eq!(pruned.magic_link_tokens, 1);
    assert_eq!(pruned.sessions, 1);
    assert_eq!(pruned.email_change_tokens, 1);

    // The counts are corroborated by database state, never trusted alone.
    assert_eq!(count(&pool, COUNT_MAGIC_LINKS).await, 1);
    assert_eq!(count(&pool, COUNT_SESSIONS).await, 1);
    assert_eq!(count(&pool, COUNT_EMAIL_CHANGES).await, 1);

    let live_session: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE token_hash = 'se-live'")
            .fetch_one(&pool)
            .await
            .expect("count live sessions");
    assert_eq!(
        live_session, 1,
        "the tick must leave a live session signed in"
    );
}

#[tokio::test]
async fn a_tick_with_nothing_dead_removes_nothing() {
    // The quiet-instance case: a sweep over a database with no dead rows is a
    // no-op that still succeeds, which is what makes hourly re-running safe.
    let (_container, pool) = migrated_pool().await;
    let clock = FakeClock::new(NOW_MILLIS);

    let account_id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, 'quiet@example.com')")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("seed account");
    sqlx::query(
        "INSERT INTO sessions (id, account_id, token_hash, last_seen_at, absolute_expires_at)
         VALUES ($1, $2, 'se-live', $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(at(NOW_MILLIS))
    .bind(at(NOW_MILLIS + 30 * DAY_MILLIS))
    .execute(&pool)
    .await
    .expect("seed live session");

    let pruned = run_retention_prune_tick(
        &MagicLinkRepo::new(pool.clone()),
        &SessionRepo::new(pool.clone()),
        &EmailChangeRepo::new(pool.clone()),
        clock.now_epoch_millis(),
        RETENTION_MILLIS,
    )
    .await
    .expect("one retention sweep");

    assert_eq!(pruned.magic_link_tokens, 0);
    assert_eq!(pruned.sessions, 0);
    assert_eq!(pruned.email_change_tokens, 0);
    assert_eq!(count(&pool, COUNT_SESSIONS).await, 1);
}
