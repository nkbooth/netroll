// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Schema integration tests for the `net_schedules` + `net_occurrences` migration:
//! run the real migrations against a fresh
//! containerized Postgres and prove the 1:1 schedule PK, the occurrence
//! UNIQUE `(definition_id, scheduled_start_at)` idempotency key, both FK
//! cascades, and the NOT NULL columns (no mocked SQL).

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

async fn insert_definition(pool: &PgPool) -> Uuid {
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
    .bind(Uuid::now_v7().simple().to_string())
    .execute(pool)
    .await
    .expect("insert minimal net definition");
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

async fn insert_occurrence(
    pool: &PgPool,
    definition_id: Uuid,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at) VALUES ($1, $2, $3)",
    )
    .bind(Uuid::now_v7())
    .bind(definition_id)
    .bind(at)
    .execute(pool)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn schedule_is_one_to_one_with_a_definition() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;

    let insert = |kind: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO net_schedules (definition_id, kind, timezone) VALUES ($1, $2, $3)",
            )
            .bind(def)
            .bind(kind)
            .bind("UTC")
            .execute(&pool)
            .await
        }
    };

    insert("recurring").await.expect("first schedule links");
    // A second schedule row for the same definition violates the PK (1:1).
    let duplicate = insert("one-off").await;
    assert!(
        duplicate.is_err(),
        "net_schedules.definition_id PK enforces one-schedule-per-definition"
    );
}

#[tokio::test]
async fn occurrence_pair_is_unique_for_idempotent_spawn() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    let at = chrono::DateTime::from_timestamp_millis(1_785_542_400_000).expect("in range");

    insert_occurrence(&pool, def, at)
        .await
        .expect("first occurrence inserts");
    // The SAME (definition_id, scheduled_start_at) pair must be rejected — this
    // constraint IS the "re-running the spawn never duplicates" guarantee.
    let duplicate = insert_occurrence(&pool, def, at).await;
    assert!(
        duplicate.is_err(),
        "UNIQUE (definition_id, scheduled_start_at) rejects a duplicate planned instant"
    );

    // A DIFFERENT instant for the same definition is allowed.
    let other = chrono::DateTime::from_timestamp_millis(1_785_628_800_000).expect("in range");
    insert_occurrence(&pool, def, other)
        .await
        .expect("a distinct instant is a distinct occurrence");
}

#[tokio::test]
async fn scheduled_start_at_is_not_null() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    let null_start = sqlx::query(
        "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at) VALUES ($1, $2, $3)",
    )
    .bind(Uuid::now_v7())
    .bind(def)
    .bind(Option::<chrono::DateTime<chrono::Utc>>::None)
    .execute(&pool)
    .await;
    assert!(null_start.is_err(), "scheduled_start_at is NOT NULL");
}

#[tokio::test]
async fn deleting_the_definition_cascades_schedule_and_occurrences_away() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    sqlx::query("INSERT INTO net_schedules (definition_id, kind, timezone) VALUES ($1, $2, $3)")
        .bind(def)
        .bind("recurring")
        .bind("UTC")
        .execute(&pool)
        .await
        .expect("link schedule");
    let at = chrono::DateTime::from_timestamp_millis(1_785_542_400_000).expect("in range");
    insert_occurrence(&pool, def, at)
        .await
        .expect("link occurrence");

    sqlx::query("DELETE FROM net_definitions WHERE id = $1")
        .bind(def)
        .execute(&pool)
        .await
        .expect("delete definition");

    let schedules: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_schedules WHERE definition_id = $1")
            .bind(def)
            .fetch_one(&pool)
            .await
            .expect("count schedules");
    let occurrences: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_occurrences WHERE definition_id = $1")
            .bind(def)
            .fetch_one(&pool)
            .await
            .expect("count occurrences");
    assert_eq!(schedules, 0, "schedule cascades with the definition");
    assert_eq!(occurrences, 0, "occurrences cascade with the definition");
}

#[tokio::test]
async fn occurrence_requires_an_existing_definition() {
    let (_container, pool) = migrated_pool().await;
    let orphan = insert_occurrence(
        &pool,
        Uuid::now_v7(),
        chrono::DateTime::from_timestamp_millis(1_785_542_400_000).expect("in range"),
    )
    .await;
    assert!(
        orphan.is_err(),
        "definition_id FK rejects an occurrence with no definition"
    );
}
