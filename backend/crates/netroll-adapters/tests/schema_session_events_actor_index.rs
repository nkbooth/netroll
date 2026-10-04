// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the partial index on `session_events (actor)` exists — the one that
//! narrows the "events this account authored" scan before the Rust-side
//! filtering of the personal-data export. Asserts the physical index in the
//! catalog, never any query text.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

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

#[tokio::test]
async fn a_partial_index_on_session_events_actor_exists() {
    let (_container, pool) = migrated_pool().await;

    // A partial index on `actor` — its definition ends in `(actor)` and carries
    // a `WHERE (actor IS NOT NULL)` predicate. The pre-existing UNIQUE
    // constraint indexes the PAIR `(session_id, seq)`, whose definition does NOT
    // contain the substring `(actor)`, so this assertion is satisfied ONLY by
    // the dedicated actor index.
    let partial_actor_indexes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes
         WHERE tablename = 'session_events'
           AND indexdef LIKE '%(actor)%'
           AND indexdef LIKE '%actor IS NOT NULL%'",
    )
    .fetch_one(&pool)
    .await
    .expect("query pg_indexes");

    assert!(
        partial_actor_indexes >= 1,
        "a partial index on session_events (actor) WHERE actor IS NOT NULL must exist for the self-check-in export query (Task 1)"
    );
}
