// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves `net_sessions` and `session_events` exist with their required columns,
//! the UNIQUE `(session_id, seq)` backstop, the NON-cascading definition FK — a
//! started session outlives definition deletion, the opposite of
//! `net_occurrences` — the cascading session FK, and the nullable posture.

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

/// Inserts a `net_sessions` row naming every non-defaulted column; success is
/// itself proof those columns exist.
async fn insert_session(pool: &PgPool, definition_id: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    // ONE real connection, not `[]` and not `{}`. An empty list is a
    // shape `connection_set_from_wire` refuses, and a bare `{}` has no
    // `connections` key at all — which is the exact marker for "this row
    // predates the connection set". A schema fixture that plants either is
    // planting a row the application refuses to read.
    sqlx::query(
        "INSERT INTO net_sessions
            (id, definition_id, definition_version, definition_snapshot,
             lifecycle, started_at)
         VALUES ($1, $2, $3, '{\"title\":\"snap\",\"connections\":[{\"id\":\"0192f4a1-0000-7000-8000-0000000000c1\",\"position\":0,\"kind\":\"echolink\",\"node\":\"12345\"}]}'::jsonb, $4, now())",
    )
    .bind(id)
    .bind(definition_id)
    .bind(1_i32)
    .bind("live")
    .execute(pool)
    .await
    .expect("insert net_session row");
    id
}

async fn insert_event(
    pool: &PgPool,
    session_id: Uuid,
    seq: i64,
    actor: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, actor, created_at)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, $5, now())",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(seq)
    .bind("session.started")
    .bind(actor)
    .execute(pool)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn both_tables_exist_with_required_columns() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    let session = insert_session(&pool, def).await;

    // The defaulted columns (last_seq DEFAULT 0, created_at/updated_at DEFAULT
    // now()) exist and carry their defaults — reading them back proves the
    // columns are present.
    let (last_seq, closed_at): (i64, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT last_seq, closed_at FROM net_sessions WHERE id = $1")
            .bind(session)
            .fetch_one(&pool)
            .await
            .expect("read net_sessions defaulted columns");
    assert_eq!(last_seq, 0, "last_seq defaults to 0");
    assert_eq!(
        closed_at, None,
        "a freshly-started session has no closed_at"
    );

    insert_event(&pool, session, 1, Some(Uuid::now_v7()))
        .await
        .expect("insert session_events row with all columns");
}

#[tokio::test]
async fn unique_session_id_seq_rejects_duplicate_pair() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    let session = insert_session(&pool, def).await;

    insert_event(&pool, session, 1, None)
        .await
        .expect("first (session, seq=1) inserts");
    let duplicate = insert_event(&pool, session, 1, None).await;
    assert!(
        duplicate.is_err(),
        "UNIQUE (session_id, seq) rejects a duplicate (session, seq) pair"
    );

    // A different seq for the same session is fine.
    insert_event(&pool, session, 2, None)
        .await
        .expect("a distinct seq is a distinct event");
}

#[tokio::test]
async fn definition_fk_is_non_cascading_so_a_session_outlives_definition_deletion() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    let session = insert_session(&pool, def).await;

    // Contrast net_occurrences (which DO cascade): a started session must
    // survive definition deletion, so the FK must refuse the delete.
    let deleted = sqlx::query("DELETE FROM net_definitions WHERE id = $1")
        .bind(def)
        .execute(&pool)
        .await;
    assert!(
        deleted.is_err(),
        "the non-cascading definition FK rejects deleting a definition with a session"
    );

    let survivors: i64 = sqlx::query_scalar("SELECT count(*) FROM net_sessions WHERE id = $1")
        .bind(session)
        .fetch_one(&pool)
        .await
        .expect("count sessions");
    assert_eq!(survivors, 1, "the session row is never cascade-deleted");
}

#[tokio::test]
async fn deleting_a_session_cascades_its_events_away() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    let session = insert_session(&pool, def).await;
    insert_event(&pool, session, 1, None)
        .await
        .expect("seed event 1");
    insert_event(&pool, session, 2, None)
        .await
        .expect("seed event 2");

    sqlx::query("DELETE FROM net_sessions WHERE id = $1")
        .bind(session)
        .execute(&pool)
        .await
        .expect("delete session");

    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM session_events WHERE session_id = $1")
            .bind(session)
            .fetch_one(&pool)
            .await
            .expect("count events");
    assert_eq!(events, 0, "session_events cascade with their net_session");
}

#[tokio::test]
async fn actor_is_nullable_but_core_columns_are_not_null() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    let session = insert_session(&pool, def).await;

    // actor NULL is allowed (system-originated events).
    insert_event(&pool, session, 1, None)
        .await
        .expect("a system event carries a NULL actor");

    // A NULL seq is rejected (NOT NULL).
    let null_seq = sqlx::query(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, created_at)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, now())",
    )
    .bind(Uuid::now_v7())
    .bind(session)
    .bind(Option::<i64>::None)
    .bind("session.started")
    .execute(&pool)
    .await;
    assert!(null_seq.is_err(), "seq is NOT NULL");

    // A NULL kind is rejected (NOT NULL).
    let null_kind = sqlx::query(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, created_at)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, now())",
    )
    .bind(Uuid::now_v7())
    .bind(session)
    .bind(5_i64)
    .bind(Option::<String>::None)
    .execute(&pool)
    .await;
    assert!(null_kind.is_err(), "kind is NOT NULL");

    // A NULL session_id is rejected (NOT NULL).
    let null_session_id = sqlx::query(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, created_at)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, now())",
    )
    .bind(Uuid::now_v7())
    .bind(Option::<Uuid>::None)
    .bind(6_i64)
    .bind("session.started")
    .execute(&pool)
    .await;
    assert!(null_session_id.is_err(), "session_id is NOT NULL");

    // A NULL payload is rejected (NOT NULL).
    let null_payload = sqlx::query(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, created_at)
         VALUES ($1, $2, $3, $4, $5, now())",
    )
    .bind(Uuid::now_v7())
    .bind(session)
    .bind(7_i64)
    .bind("session.started")
    .bind(Option::<serde_json::Value>::None)
    .execute(&pool)
    .await;
    assert!(null_payload.is_err(), "payload is NOT NULL");
}
