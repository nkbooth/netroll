// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Schema integration tests for the `accounts.callsign` migration: run the
//! real migrations against a fresh containerized
//! Postgres and prove the partial unique index is the whole invariant —
//! many NULLs coexist, non-null duplicates collide, and freeing a value by
//! changing it away makes it reusable (no mocked SQL).

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

async fn insert_account(
    pool: &PgPool,
    email: &str,
    callsign: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email, callsign) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(email)
        .bind(callsign)
        .execute(pool)
        .await
        .map(|_| id)
}

#[tokio::test]
async fn two_accounts_cannot_share_the_same_non_null_callsign() {
    let (_container, pool) = migrated_pool().await;

    insert_account(&pool, "a@example.com", Some("W1AW"))
        .await
        .expect("first claim of a callsign succeeds");

    let collision = insert_account(&pool, "b@example.com", Some("W1AW")).await;
    assert!(
        collision.is_err(),
        "a second account claiming the same non-null callsign must violate the unique index"
    );
}

#[tokio::test]
async fn many_accounts_with_null_callsign_coexist() {
    let (_container, pool) = migrated_pool().await;

    insert_account(&pool, "a@example.com", None)
        .await
        .expect("account without a callsign succeeds");
    insert_account(&pool, "b@example.com", None)
        .await
        .expect("a second account without a callsign also succeeds — partial index excludes NULL");
    insert_account(&pool, "c@example.com", None)
        .await
        .expect("a third account without a callsign also succeeds");
}

#[tokio::test]
async fn changing_a_callsign_away_frees_it_for_another_account() {
    let (_container, pool) = migrated_pool().await;

    let account_a = insert_account(&pool, "a@example.com", Some("W1AW"))
        .await
        .expect("account A claims W1AW");

    sqlx::query("UPDATE accounts SET callsign = $2 WHERE id = $1")
        .bind(account_a)
        .bind("K1ABC")
        .execute(&pool)
        .await
        .expect("account A changes to a different callsign");

    insert_account(&pool, "b@example.com", Some("W1AW"))
        .await
        .expect("W1AW is freed once A no longer holds it, so B can claim it");
}
