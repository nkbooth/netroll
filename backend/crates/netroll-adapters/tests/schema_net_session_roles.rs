// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the `net_session_roles` constraints against a real containerized
//! Postgres: one role per (session, account), both FKs cascade, role is NOT
//! NULL.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use uuid::Uuid;

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

async fn insert_account(pool: &PgPool, email: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, $2)")
        .bind(id)
        .bind(email)
        .execute(pool)
        .await
        .expect("seed account");
    id
}

/// Seeds a definition + a session and returns the session id. A session row
/// needs a definition (FK) and the frozen snapshot columns the start path
/// writes; the test inserts the minimum a role grant depends on.
async fn insert_session(pool: &PgPool, owner: Uuid) -> Uuid {
    let definition_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO net_definitions
           (id, title, net_category, net_type, link_token)
         VALUES ($1, 'Net', 'traffic', 'open', $2)",
    )
    .bind(definition_id)
    .bind(definition_id.to_string())
    .execute(pool)
    .await
    .expect("seed definition");
    // Every definition is born with at least one connection, and
    // the definition row carries no connection fact of its
    // own — a row alone is a net the application refuses to read.
    sqlx::query(
        "INSERT INTO net_connections
            (id, definition_id, position, kind, planned_frequency_hz, band, mode)
         VALUES ($1, $2, 0, 'hf', 14230000, '20m', 'ssb')",
    )
    .bind(Uuid::now_v7())
    .bind(definition_id)
    .execute(pool)
    .await
    .expect("seed the definition's one connection");
    sqlx::query(
        "INSERT INTO net_definition_owners (net_definition_id, account_id) VALUES ($1, $2)",
    )
    .bind(definition_id)
    .bind(owner)
    .execute(pool)
    .await
    .expect("seed owner");

    let session_id = Uuid::now_v7();
    // ONE real connection, not `[]` and not `{}`. An empty list is a
    // shape `connection_set_from_wire` refuses, and a bare `{}` has no
    // `connections` key at all — which is the exact marker for "this row
    // predates the connection set". A schema fixture that plants either is
    // planting a row the application refuses to read.
    sqlx::query(
        "INSERT INTO net_sessions
           (id, definition_id, definition_version, definition_snapshot,
            lifecycle, last_seq)
         VALUES ($1, $2, 1, '{\"title\":\"snap\",\"connections\":[{\"id\":\"0192f4a1-0000-7000-8000-0000000000c1\",\"position\":0,\"kind\":\"echolink\",\"node\":\"12345\"}]}'::jsonb, 'live', 1)",
    )
    .bind(session_id)
    .bind(definition_id)
    .execute(pool)
    .await
    .expect("seed session");
    session_id
}

async fn insert_role(
    pool: &PgPool,
    session_id: Uuid,
    account_id: Uuid,
    role: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO net_session_roles (net_session_id, account_id, role, granted_by)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(session_id)
    .bind(account_id)
    .bind(role)
    .bind(account_id)
    .execute(pool)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn one_role_row_per_session_and_account() {
    let (_container, pool) = migrated_pool().await;
    let owner = insert_account(&pool, "owner@example.com").await;
    let staff = insert_account(&pool, "staff@example.com").await;
    let session = insert_session(&pool, owner).await;

    insert_role(&pool, session, staff, "logger")
        .await
        .expect("first grant for a (session, account) succeeds");
    let duplicate = insert_role(&pool, session, staff, "relay").await;
    assert!(
        duplicate.is_err(),
        "the same (session, account) must hit the primary key"
    );
}

#[tokio::test]
async fn a_grant_requires_an_existing_session() {
    let (_container, pool) = migrated_pool().await;
    let account = insert_account(&pool, "staff@example.com").await;

    let orphan = insert_role(&pool, Uuid::now_v7(), account, "logger").await;
    assert!(
        orphan.is_err(),
        "a grant without a valid net_session_id must be rejected by the FK"
    );
}

#[tokio::test]
async fn a_grant_requires_an_existing_account() {
    let (_container, pool) = migrated_pool().await;
    let owner = insert_account(&pool, "owner@example.com").await;
    let session = insert_session(&pool, owner).await;

    let orphan = insert_role(&pool, session, Uuid::now_v7(), "logger").await;
    assert!(
        orphan.is_err(),
        "a grant without a valid account_id must be rejected by the FK"
    );
}

#[tokio::test]
async fn deleting_the_account_cascades_its_grants() {
    let (_container, pool) = migrated_pool().await;
    let owner = insert_account(&pool, "owner@example.com").await;
    let staff = insert_account(&pool, "staff@example.com").await;
    let session = insert_session(&pool, owner).await;
    insert_role(&pool, session, staff, "logger")
        .await
        .expect("grant");

    // The account-finalize path deletes by accounts(id); the FK must
    // cascade or that DELETE would be blocked.
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(staff)
        .execute(&pool)
        .await
        .expect("account delete cascades its grants");

    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_session_roles WHERE account_id = $1")
            .bind(staff)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(remaining, 0, "the staff account's grant was cascaded away");
}

#[tokio::test]
async fn deleting_the_session_cascades_its_grants() {
    let (_container, pool) = migrated_pool().await;
    let owner = insert_account(&pool, "owner@example.com").await;
    let staff = insert_account(&pool, "staff@example.com").await;
    let session = insert_session(&pool, owner).await;
    insert_role(&pool, session, staff, "logger")
        .await
        .expect("grant");

    sqlx::query("DELETE FROM net_sessions WHERE id = $1")
        .bind(session)
        .execute(&pool)
        .await
        .expect("session delete cascades its grants");

    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_session_roles WHERE net_session_id = $1")
            .bind(session)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(remaining, 0, "the session's grant was cascaded away");
}

#[tokio::test]
async fn a_role_is_mandatory() {
    let (_container, pool) = migrated_pool().await;
    let owner = insert_account(&pool, "owner@example.com").await;
    let staff = insert_account(&pool, "staff@example.com").await;
    let session = insert_session(&pool, owner).await;

    let missing_role = sqlx::query(
        "INSERT INTO net_session_roles (net_session_id, account_id, role)
         VALUES ($1, $2, NULL)",
    )
    .bind(session)
    .bind(staff)
    .execute(&pool)
    .await;
    assert!(
        missing_role.is_err(),
        "a grant with no role is not a grant (NOT NULL)"
    );
}
