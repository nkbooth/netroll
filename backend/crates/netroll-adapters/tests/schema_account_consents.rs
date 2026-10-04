// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the `account_consents` constraints against a real containerized
//! Postgres: one consent per (account, version), FK integrity, and a NOT NULL
//! consent timestamp.

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

async fn insert_consent(
    pool: &PgPool,
    account_id: Uuid,
    terms_version: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO account_consents (id, account_id, terms_version, consented_at)
         VALUES ($1, $2, $3, now())",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(terms_version)
    .execute(pool)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn one_consent_row_per_account_and_version() {
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;

    insert_consent(&pool, account_id, "2026-07-15")
        .await
        .expect("first consent for a version succeeds");
    let duplicate = insert_consent(&pool, account_id, "2026-07-15").await;
    assert!(
        duplicate.is_err(),
        "same (account_id, terms_version) must hit the unique constraint"
    );

    // A different version is a separate acceptance record (history is
    // preserved across bumps — that is the point of the table).
    insert_consent(&pool, account_id, "2027-01-01")
        .await
        .expect("a later terms version records separately");
}

#[tokio::test]
async fn consents_require_an_existing_account() {
    let (_container, pool) = migrated_pool().await;

    let orphan = insert_consent(&pool, Uuid::now_v7(), "2026-07-15").await;

    assert!(
        orphan.is_err(),
        "a consent row without a valid account_id must be rejected by the FK"
    );
}

#[tokio::test]
async fn consented_at_is_mandatory() {
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;

    let missing_timestamp = sqlx::query(
        "INSERT INTO account_consents (id, account_id, terms_version, consented_at)
         VALUES ($1, $2, $3, NULL)",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind("2026-07-15")
    .execute(&pool)
    .await;

    assert!(
        missing_timestamp.is_err(),
        "consent without a timestamp is not a recorded consent (NOT NULL)"
    );
}
