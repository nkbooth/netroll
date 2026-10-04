// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the `net_definitions` and `net_definition_owners` shape against a real
//! containerized Postgres: the version default, the NOT NULL and nullable
//! columns, the composite owner PK, and — critically — that BOTH FKs cascade.
//! The `account_id` CASCADE is what keeps a single `DELETE FROM accounts`
//! unblocked.

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

/// Inserts a minimal net definition (only the NOT NULL columns), returning
/// its id. `link_token` is NOT NULL with no DB default (app-minted), so every
/// insert must supply a unique one.
async fn insert_definition(pool: &PgPool) -> Uuid {
    insert_definition_with_token(pool, &Uuid::now_v7().simple().to_string()).await
}

/// Inserts a minimal net definition with an explicit `link_token`.
async fn insert_definition_with_token(pool: &PgPool, link_token: &str) -> Uuid {
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
    .bind(link_token)
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

#[tokio::test]
async fn definition_version_defaults_to_one_and_optionals_round_trip() {
    let (_container, pool) = migrated_pool().await;
    let id = insert_definition(&pool).await;

    let row = sqlx::query(
        "SELECT definition_version, description, grid, expected_duration_minutes
         FROM net_definitions WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("select the row back");
    let version: i32 = row.get("definition_version");
    assert_eq!(version, 1, "definition_version defaults to 1");
    let description: Option<String> = row.get("description");
    assert_eq!(description, None, "optional columns default NULL");

    // Optional columns accept values and round-trip. (`repeater_offset_hz` used
    // to be the fourth; it left with the flat connection columns
    // and now lives on `net_connections`.)
    sqlx::query(
        "UPDATE net_definitions
         SET description = $2, grid = $3, expected_duration_minutes = $4
         WHERE id = $1",
    )
    .bind(id)
    .bind("Weekly NTS")
    .bind("FN31pr")
    .bind(90_i32)
    .execute(&pool)
    .await
    .expect("optional columns accept values");

    let row = sqlx::query(
        "SELECT description, grid, expected_duration_minutes
         FROM net_definitions WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("re-select");
    assert_eq!(
        row.get::<Option<String>, _>("description"),
        Some("Weekly NTS".to_owned())
    );
    assert_eq!(
        row.get::<Option<i32>, _>("expected_duration_minutes"),
        Some(90)
    );
}

#[tokio::test]
async fn visibility_defaults_to_listed_and_unlisted_round_trips() {
    let (_container, pool) = migrated_pool().await;
    // An insert omitting `visibility` lands the column DEFAULT 'listed' (a
    // DB-level default; also backfills older rows).
    let id = insert_definition(&pool).await;
    let visibility: String =
        sqlx::query_scalar("SELECT visibility FROM net_definitions WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("select visibility");
    assert_eq!(
        visibility, "listed",
        "omitted visibility defaults to listed"
    );

    // An explicit 'unlisted' round-trips.
    sqlx::query("UPDATE net_definitions SET visibility = 'unlisted' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("set unlisted");
    let visibility: String =
        sqlx::query_scalar("SELECT visibility FROM net_definitions WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("re-select visibility");
    assert_eq!(visibility, "unlisted");
}

#[tokio::test]
async fn link_token_is_not_null_and_unique() {
    let (_container, pool) = migrated_pool().await;
    insert_definition_with_token(&pool, "shared-token").await;

    // A second row with the SAME link_token violates the unique index.
    let duplicate = sqlx::query(
        "INSERT INTO net_definitions
            (id, title, net_category, net_type, link_token)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind("Dup")
    .bind("traffic")
    .bind("open")
    .bind("shared-token")
    .execute(&pool)
    .await;
    assert!(
        duplicate.is_err(),
        "the unique index on link_token must reject a duplicate token"
    );

    // A NULL link_token is rejected (NOT NULL).
    let null_token = sqlx::query(
        "INSERT INTO net_definitions
            (id, title, net_category, net_type, link_token)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind("NoToken")
    .bind("traffic")
    .bind("open")
    .bind(Option::<String>::None)
    .execute(&pool)
    .await;
    assert!(null_token.is_err(), "link_token is NOT NULL");
}

#[tokio::test]
async fn archived_at_is_nullable_and_defaults_to_null() {
    // The ownership-lifecycle soft-delete column. A
    // freshly inserted definition is active (archived_at IS NULL) and the
    // column accepts a timestamp on archival.
    let (_container, pool) = migrated_pool().await;
    let id = insert_definition(&pool).await;

    let archived_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT archived_at FROM net_definitions WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("select archived_at");
    assert_eq!(archived_at, None, "a fresh definition is not archived");

    // The column accepts a timestamp (round-trips on archival).
    sqlx::query("UPDATE net_definitions SET archived_at = now() WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("archived_at accepts a timestamp");
    let archived_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT archived_at FROM net_definitions WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("re-select archived_at");
    assert!(archived_at.is_some(), "archived_at round-trips a timestamp");
}

#[tokio::test]
async fn owner_link_has_a_composite_pk_rejecting_duplicate_owners() {
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "owner@example.com").await;

    let insert_owner = |acc: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO net_definition_owners (net_definition_id, account_id) VALUES ($1, $2)",
            )
            .bind(def_id)
            .bind(acc)
            .execute(&pool)
            .await
        }
    };

    insert_owner(account_id).await.expect("first owner links");
    let duplicate = insert_owner(account_id).await;
    assert!(
        duplicate.is_err(),
        "the (net_definition_id, account_id) PK must reject a duplicate owner"
    );
}

#[tokio::test]
async fn deleting_the_account_cascades_the_owner_link_away() {
    // The account-deletion contract: a single DELETE FROM accounts must not
    // be blocked by this FK — it cascades.
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "departing@example.com").await;
    sqlx::query(
        "INSERT INTO net_definition_owners (net_definition_id, account_id) VALUES ($1, $2)",
    )
    .bind(def_id)
    .bind(account_id)
    .execute(&pool)
    .await
    .expect("link owner");

    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("deleting the account must NOT be blocked by the owner FK (ON DELETE CASCADE)");

    let owner_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_definition_owners WHERE account_id = $1")
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .expect("count owner rows");
    assert_eq!(owner_rows, 0, "the owner link cascaded away");
    // The definition itself survives (a departed sole owner leaves a
    // zero-owner row the archival path handles — NOT a cascade delete of the net).
    let def_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM net_definitions WHERE id = $1")
        .bind(def_id)
        .fetch_one(&pool)
        .await
        .expect("count definition rows");
    assert_eq!(
        def_rows, 1,
        "the definition is NOT deleted with the account"
    );
}

#[tokio::test]
async fn deleting_the_definition_cascades_its_owner_links_away() {
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "owner2@example.com").await;
    sqlx::query(
        "INSERT INTO net_definition_owners (net_definition_id, account_id) VALUES ($1, $2)",
    )
    .bind(def_id)
    .bind(account_id)
    .execute(&pool)
    .await
    .expect("link owner");

    sqlx::query("DELETE FROM net_definitions WHERE id = $1")
        .bind(def_id)
        .execute(&pool)
        .await
        .expect("delete definition");

    let owner_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM net_definition_owners WHERE net_definition_id = $1",
    )
    .bind(def_id)
    .fetch_one(&pool)
    .await
    .expect("count owner rows");
    assert_eq!(
        owner_rows, 0,
        "owner links cascade when the definition is deleted"
    );
}

#[tokio::test]
async fn owner_link_requires_an_existing_definition_and_account() {
    let (_container, pool) = migrated_pool().await;
    let def_id = insert_definition(&pool).await;
    let account_id = insert_account(&pool, "owner3@example.com").await;

    let orphan_account = sqlx::query(
        "INSERT INTO net_definition_owners (net_definition_id, account_id) VALUES ($1, $2)",
    )
    .bind(def_id)
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert!(
        orphan_account.is_err(),
        "account_id FK rejects a missing account"
    );

    let orphan_definition = sqlx::query(
        "INSERT INTO net_definition_owners (net_definition_id, account_id) VALUES ($1, $2)",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .execute(&pool)
    .await;
    assert!(
        orphan_definition.is_err(),
        "net_definition_id FK rejects a missing definition"
    );
}
