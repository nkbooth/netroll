// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the accounts/auth constraints against a real containerized Postgres:
//! unique email, kind vocabulary, FK integrity.

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

async fn insert_account(pool: &PgPool, email: &str) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, $2)")
        .bind(id)
        .bind(email)
        .execute(pool)
        .await
        .map(|_| id)
}

#[tokio::test]
async fn accounts_reject_duplicate_emails() {
    let (_container, pool) = migrated_pool().await;

    insert_account(&pool, "op@example.com")
        .await
        .expect("first insert succeeds");
    let duplicate = insert_account(&pool, "op@example.com").await;

    assert!(
        duplicate.is_err(),
        "second insert with the same email must hit the unique constraint"
    );
}

#[tokio::test]
async fn auth_methods_accept_magic_link_kind_and_reject_unknown_kinds() {
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com")
        .await
        .expect("account insert");

    let insert_kind = "INSERT INTO auth_methods (id, account_id, kind) VALUES ($1, $2, $3)";
    sqlx::query(insert_kind)
        .bind(Uuid::now_v7())
        .bind(account_id)
        .bind("magic-link")
        .execute(&pool)
        .await
        .expect("'magic-link' is an accepted kind");

    let unknown = sqlx::query(insert_kind)
        .bind(Uuid::now_v7())
        .bind(account_id)
        .bind("carrier-pigeon")
        .execute(&pool)
        .await;
    assert!(
        unknown.is_err(),
        "an unknown kind must be rejected by the CHECK constraint"
    );
}

#[tokio::test]
async fn auth_methods_require_an_existing_account() {
    let (_container, pool) = migrated_pool().await;

    let orphan = sqlx::query("INSERT INTO auth_methods (id, account_id, kind) VALUES ($1, $2, $3)")
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .bind("magic-link")
        .execute(&pool)
        .await;

    assert!(
        orphan.is_err(),
        "an auth_methods row without a valid account_id must be rejected by the FK"
    );
}
