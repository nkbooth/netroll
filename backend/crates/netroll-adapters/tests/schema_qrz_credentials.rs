// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the 1:1-per-account `qrz_credentials` store round-trips opaque
//! sealed bytes, replaces in place, deletes, reflects `is_set` and cascades on
//! account deletion, against a real containerized Postgres. The repo moves
//! opaque bytes only — it performs no crypto.

use netroll_adapters::pg::qrz_credentials::QrzCredentialRepo;
use netroll_domain::qrz::SealedQrzCredentials;
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

/// A distinct sealed-credential fixture; `tag` varies every byte field so two
/// fixtures never collide (the repo stores whatever opaque bytes it is given).
fn sample_sealed(tag: u8) -> SealedQrzCredentials {
    SealedQrzCredentials {
        wrapped_dek: vec![tag; 48],
        dek_nonce: vec![tag.wrapping_add(1); 12],
        credential_ciphertext: vec![tag.wrapping_add(2); 64],
        credential_nonce: vec![tag.wrapping_add(3); 12],
        kek_version: 1,
    }
}

#[tokio::test]
async fn set_then_get_round_trips_the_sealed_bytes() {
    let (_c, pool) = migrated_pool().await;
    let repo = QrzCredentialRepo::new(pool.clone());
    let account = insert_account(&pool, "op@example.com").await;

    assert!(repo.get(account).await.expect("get").is_none());
    assert!(!repo.is_set(account).await.expect("is_set"));

    let sealed = sample_sealed(10);
    repo.set(account, &sealed).await.expect("set");

    assert!(repo.is_set(account).await.expect("is_set"));
    let fetched = repo.get(account).await.expect("get").expect("row present");
    assert_eq!(fetched, sealed, "the stored bytes round-trip verbatim");
}

#[tokio::test]
async fn replace_keeps_one_row_and_swaps_the_ciphertext() {
    let (_c, pool) = migrated_pool().await;
    let repo = QrzCredentialRepo::new(pool.clone());
    let account = insert_account(&pool, "op@example.com").await;

    repo.set(account, &sample_sealed(10))
        .await
        .expect("first set");
    let replacement = sample_sealed(200);
    repo.set(account, &replacement).await.expect("replace");

    // Still exactly one row (1:1 per account), now carrying the fresh material.
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM qrz_credentials WHERE account_id = $1")
            .bind(account)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(
        count, 1,
        "PUT replaces the row rather than inserting a second"
    );
    assert_eq!(
        repo.get(account).await.expect("get").expect("row"),
        replacement,
        "no stale key material lingers after replace"
    );
}

#[tokio::test]
async fn delete_removes_the_row_and_clears_is_set() {
    let (_c, pool) = migrated_pool().await;
    let repo = QrzCredentialRepo::new(pool.clone());
    let account = insert_account(&pool, "op@example.com").await;

    repo.set(account, &sample_sealed(10)).await.expect("set");
    repo.delete(account).await.expect("delete");

    assert!(!repo.is_set(account).await.expect("is_set"));
    assert!(repo.get(account).await.expect("get").is_none());
    // Idempotent: deleting an absent row is a no-op success.
    repo.delete(account).await.expect("delete is idempotent");
}

#[tokio::test]
async fn account_deletion_cascades_to_the_credential_row() {
    let (_c, pool) = migrated_pool().await;
    let repo = QrzCredentialRepo::new(pool.clone());
    let account = insert_account(&pool, "op@example.com").await;
    repo.set(account, &sample_sealed(10)).await.expect("set");

    // Account finalize is a single `DELETE FROM accounts`; the credential
    // row must cascade rather than block it (ON DELETE CASCADE).
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(account)
        .execute(&pool)
        .await
        .expect("delete account cascades");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM qrz_credentials WHERE account_id = $1")
            .bind(account)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(count, 0, "the credential row is removed with the account");
}

#[tokio::test]
async fn a_credential_row_requires_an_existing_account() {
    let (_c, pool) = migrated_pool().await;
    let repo = QrzCredentialRepo::new(pool.clone());

    let orphan = repo.set(Uuid::now_v7(), &sample_sealed(10)).await;
    assert!(
        orphan.is_err(),
        "a credential row without a valid account_id must be rejected by the FK"
    );
}
