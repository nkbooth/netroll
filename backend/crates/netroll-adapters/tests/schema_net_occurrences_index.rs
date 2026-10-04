// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the single-column index on `net_occurrences (scheduled_start_at)`
//! exists — the one the upcoming-discovery query orders and range-filters on.
//! Asserts the physical index in the catalog, never any query text.

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
async fn a_single_column_index_on_scheduled_start_at_exists() {
    let (_container, pool) = migrated_pool().await;

    // A single-column index on `scheduled_start_at` — its definition ends in
    // `(scheduled_start_at)`. The pre-existing UNIQUE constraint indexes the
    // PAIR `(definition_id, scheduled_start_at)`, whose definition does NOT
    // contain the substring `(scheduled_start_at)` (the leading column is
    // `definition_id`), so this assertion is satisfied ONLY by the dedicated
    // discovery ordering index.
    let single_column_indexes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes
         WHERE tablename = 'net_occurrences'
           AND indexdef LIKE '%(scheduled_start_at)'",
    )
    .fetch_one(&pool)
    .await
    .expect("query pg_indexes");

    assert!(
        single_column_indexes >= 1,
        "a dedicated index on net_occurrences (scheduled_start_at) must exist for discovery ordering"
    );
}
