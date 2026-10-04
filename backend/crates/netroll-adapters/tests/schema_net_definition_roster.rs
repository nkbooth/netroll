// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves `net_definition_roster` exists keyed by `(definition_id, callsign)`
//! with the NON-cascading definition FK, against a real containerized Postgres
//! rather than the text of a `.sql` file. The one-time backfill replays that
//! used to live here are RETIRED: the historical set is folded into one initial
//! schema, and a new database starts empty, so they can no longer run.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use uuid::Uuid;

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

async fn insert_definition(pool: &PgPool, token: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO net_definitions
            (id, title, net_category, net_type, link_token)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind("Sunday Traffic Net")
    .bind("traffic")
    .bind("open")
    .bind(token)
    .execute(pool)
    .await
    .expect("insert definition");
    // Every definition is born with at least one connection, and
    // the definition row itself carries no connection fact —
    // so a fixture that inserts the row alone builds a net the application
    // refuses to read. One HF way, at the export position.
    sqlx::query(
        "INSERT INTO net_connections
            (id, definition_id, position, kind, planned_frequency_hz, band, mode)
         VALUES ($1, $2, 0, 'hf', 14230000, '20m', 'ssb')",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(pool)
    .await
    .expect("insert the definition's one connection");
    id
}

#[tokio::test]
async fn the_table_exists_and_the_definition_fk_is_non_cascading() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool, "tok-shape").await;

    // Inserting a row naming every non-defaulted column proves the columns +
    // (definition_id, callsign) PK exist.
    sqlx::query(
        "INSERT INTO net_definition_roster
            (definition_id, callsign, name, location, last_seen_at)
         VALUES ($1, 'W1AW', 'Maria', 'Hartford, CT', now())",
    )
    .bind(def)
    .execute(&pool)
    .await
    .expect("insert roster-memory row");

    // The definition FK is NON-cascading (NO ACTION): a definition with roster
    // memory cannot be hard-deleted out from under it (matching net_sessions).
    let deleted = sqlx::query("DELETE FROM net_definitions WHERE id = $1")
        .bind(def)
        .execute(&pool)
        .await;
    assert!(
        deleted.is_err(),
        "the non-cascading definition FK rejects deleting a definition with roster memory"
    );
}
