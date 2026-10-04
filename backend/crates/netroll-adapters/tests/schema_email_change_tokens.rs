// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the `email_change_tokens` constraints against a real containerized
//! Postgres: FK to accounts, UNIQUE `token_hash`, and deliberately NO
//! per-account uniqueness — multiple pending tokens coexist and the
//! last-confirmed one wins.

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

async fn insert_token(
    pool: &PgPool,
    account_id: Uuid,
    new_email: &str,
    token_hash: &[u8],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_change_tokens (id, account_id, new_email, token_hash, expires_at)
         VALUES ($1, $2, $3, $4, now() + interval '15 minutes')",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(new_email)
    .bind(token_hash)
    .execute(pool)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn a_valid_token_inserts_and_round_trips() {
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;

    insert_token(&pool, account_id, "new@example.com", b"hash-a")
        .await
        .expect("a token with a valid account_id inserts");

    let (stored_account, stored_email): (Uuid, String) = sqlx::query_as(
        "SELECT account_id, new_email FROM email_change_tokens WHERE token_hash = $1",
    )
    .bind(&b"hash-a"[..])
    .fetch_one(&pool)
    .await
    .expect("stored row round-trips");
    assert_eq!(stored_account, account_id);
    assert_eq!(stored_email, "new@example.com");

    let unconsumed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM email_change_tokens
         WHERE token_hash = $1 AND consumed_at IS NULL",
    )
    .bind(&b"hash-a"[..])
    .fetch_one(&pool)
    .await
    .expect("count unconsumed");
    assert_eq!(unconsumed, 1, "a freshly issued token is unconsumed");
}

#[tokio::test]
async fn tokens_require_an_existing_account() {
    let (_container, pool) = migrated_pool().await;

    let orphan = insert_token(&pool, Uuid::now_v7(), "new@example.com", b"hash-orphan").await;

    assert!(
        orphan.is_err(),
        "a token row without a valid account_id must be rejected by the FK"
    );
}

#[tokio::test]
async fn duplicate_token_hashes_are_rejected() {
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;

    insert_token(&pool, account_id, "new@example.com", b"same-hash")
        .await
        .expect("first token with this hash inserts");
    let duplicate = insert_token(&pool, account_id, "other@example.com", b"same-hash").await;

    assert!(
        duplicate.is_err(),
        "a second row with the same token_hash must hit the UNIQUE constraint"
    );
}

#[tokio::test]
async fn two_pending_tokens_for_the_same_account_coexist() {
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;

    // No per-account uniqueness (deliberate — each token is single-use with
    // a 15-minute TTL, last-confirmed wins; cancellation machinery is YAGNI).
    insert_token(&pool, account_id, "first@example.com", b"hash-1")
        .await
        .expect("first pending token inserts");
    insert_token(&pool, account_id, "second@example.com", b"hash-2")
        .await
        .expect("a second pending token for the same account also inserts");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM email_change_tokens WHERE account_id = $1")
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .expect("count tokens");
    assert_eq!(count, 2, "both pending tokens coexist");
}
