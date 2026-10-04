// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the `net_favorites` shape against a real containerized Postgres: the
//! composite-PK idempotency key rejects a duplicate favorite, the `account_id` FK
//! CASCADEs (keeping a single `DELETE FROM accounts` unblocked), both FKs
//! reject orphan rows, and `created_at` defaults to `now()`.

use sqlx::PgPool;
use sqlx::Row;
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

/// Inserts a minimal net definition (only the NOT NULL columns), returning its
/// id — `link_token` is NOT NULL with no DB default, so every insert supplies a
/// unique one.
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

async fn insert_favorite(
    pool: &PgPool,
    account_id: Uuid,
    net_definition_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO net_favorites (account_id, net_definition_id) VALUES ($1, $2)")
        .bind(account_id)
        .bind(net_definition_id)
        .execute(pool)
        .await
        .map(|_| ())
}

#[tokio::test]
async fn favorite_has_a_composite_pk_rejecting_duplicate_rows() {
    // The (account_id, net_definition_id) PK is the idempotency key an
    // ON CONFLICT DO NOTHING favorite relies on.
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "fav@example.com").await;

    insert_favorite(&pool, account_id, def_id)
        .await
        .expect("first favorite inserts");
    let duplicate = insert_favorite(&pool, account_id, def_id).await;
    assert!(
        duplicate.is_err(),
        "the (account_id, net_definition_id) PK must reject a duplicate favorite"
    );
}

#[tokio::test]
async fn deleting_the_account_cascades_the_favorite_away() {
    // The account-deletion contract: a single DELETE FROM accounts must not be blocked by
    // this FK — it cascades. The net definition itself survives.
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "departing@example.com").await;
    insert_favorite(&pool, account_id, def_id)
        .await
        .expect("favorite the net");

    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("deleting the account must NOT be blocked by the favorite FK (ON DELETE CASCADE)");

    let favorite_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_favorites WHERE account_id = $1")
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .expect("count favorite rows");
    assert_eq!(favorite_rows, 0, "the favorite cascaded away");

    let def_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM net_definitions WHERE id = $1")
        .bind(def_id)
        .fetch_one(&pool)
        .await
        .expect("count definition rows");
    assert_eq!(
        def_rows, 1,
        "the favorited net is NOT deleted with the account"
    );
}

#[tokio::test]
async fn deleting_the_definition_cascades_its_favorites_away() {
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "keeper@example.com").await;
    insert_favorite(&pool, account_id, def_id)
        .await
        .expect("favorite the net");

    sqlx::query("DELETE FROM net_definitions WHERE id = $1")
        .bind(def_id)
        .execute(&pool)
        .await
        .expect("delete definition");

    let favorite_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_favorites WHERE net_definition_id = $1")
            .bind(def_id)
            .fetch_one(&pool)
            .await
            .expect("count favorite rows");
    assert_eq!(
        favorite_rows, 0,
        "favorites cascade when the definition is deleted"
    );
}

#[tokio::test]
async fn favorite_requires_an_existing_account_and_definition() {
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "real@example.com").await;

    let orphan_account = insert_favorite(&pool, Uuid::now_v7(), def_id).await;
    assert!(
        orphan_account.is_err(),
        "account_id FK rejects a missing account"
    );

    let orphan_definition = insert_favorite(&pool, account_id, Uuid::now_v7()).await;
    assert!(
        orphan_definition.is_err(),
        "net_definition_id FK rejects a missing definition"
    );
}

#[tokio::test]
async fn favorite_created_at_defaults_to_now() {
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "timed@example.com").await;
    insert_favorite(&pool, account_id, def_id)
        .await
        .expect("favorite the net");

    let row = sqlx::query(
        "SELECT created_at FROM net_favorites WHERE account_id = $1 AND net_definition_id = $2",
    )
    .bind(account_id)
    .bind(def_id)
    .fetch_one(&pool)
    .await
    .expect("select the favorite back");
    let created_at: Option<chrono::DateTime<chrono::Utc>> = row.get("created_at");
    assert!(
        created_at.is_some(),
        "created_at is populated by the now() default"
    );
}
