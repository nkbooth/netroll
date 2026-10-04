// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the `net_delivery_configs` shape against a real containerized
//! Postgres: the 1:1 PK, the `delivery_emails text[]` default-empty set, the
//! nullable `webhook_url` and `webhook_secret` columns, and the FK cascade from
//! `net_definitions`.

use sqlx::PgPool;
use sqlx::Row;
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

#[tokio::test]
async fn delivery_config_is_one_to_one_with_a_definition() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;

    let insert = || {
        let pool = pool.clone();
        async move {
            sqlx::query("INSERT INTO net_delivery_configs (definition_id) VALUES ($1)")
                .bind(def)
                .execute(&pool)
                .await
        }
    };

    insert().await.expect("first config row inserts");
    // The PRIMARY KEY on definition_id makes a definition 1:1 with its config.
    let second = insert().await;
    assert!(
        second.is_err(),
        "a second delivery-config row for the same definition must violate the PK"
    );
}

#[tokio::test]
async fn delivery_emails_defaults_to_the_empty_set_and_webhook_columns_are_nullable() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;

    // Insert only the key: emails must default to '{}', and webhook_url,
    // webhook_secret and discord_webhook_url must all be NULL-able (the
    // "delivery off" state; the third destination came later).
    sqlx::query("INSERT INTO net_delivery_configs (definition_id) VALUES ($1)")
        .bind(def)
        .execute(&pool)
        .await
        .expect("insert config with defaults only");

    let row = sqlx::query(
        "SELECT delivery_emails, webhook_url, webhook_secret, discord_webhook_url
         FROM net_delivery_configs WHERE definition_id = $1",
    )
    .bind(def)
    .fetch_one(&pool)
    .await
    .expect("read back the config row");

    let emails: Vec<String> = row.get("delivery_emails");
    let webhook_url: Option<String> = row.get("webhook_url");
    let webhook_secret: Option<String> = row.get("webhook_secret");
    let discord_webhook_url: Option<String> = row.get("discord_webhook_url");
    assert!(
        emails.is_empty(),
        "delivery_emails defaults to the empty set"
    );
    assert!(webhook_url.is_none(), "webhook_url is nullable / unset");
    assert!(
        webhook_secret.is_none(),
        "webhook_secret is nullable / unset"
    );
    assert!(
        discord_webhook_url.is_none(),
        "discord_webhook_url is nullable with NO default — an existing row is \
         unaffected by the migration"
    );
}

#[tokio::test]
async fn delivery_emails_round_trips_a_text_array() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;

    let emails = vec!["a@example.com".to_owned(), "b@example.com".to_owned()];
    sqlx::query(
        "INSERT INTO net_delivery_configs
             (definition_id, delivery_emails, webhook_url, webhook_secret, discord_webhook_url)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(def)
    .bind(&emails)
    .bind("https://example.com/hook")
    .bind("secret-value")
    .bind("https://discord.com/api/webhooks/1/tok")
    .execute(&pool)
    .await
    .expect("insert config with a text[] of emails");

    let row = sqlx::query(
        "SELECT delivery_emails, webhook_url, webhook_secret, discord_webhook_url
         FROM net_delivery_configs WHERE definition_id = $1",
    )
    .bind(def)
    .fetch_one(&pool)
    .await
    .expect("read back");
    let stored: Vec<String> = row.get("delivery_emails");
    let webhook_url: Option<String> = row.get("webhook_url");
    let webhook_secret: Option<String> = row.get("webhook_secret");
    let discord_webhook_url: Option<String> = row.get("discord_webhook_url");
    assert_eq!(stored, emails, "the text[] round-trips exactly");
    assert_eq!(webhook_url.as_deref(), Some("https://example.com/hook"));
    assert_eq!(webhook_secret.as_deref(), Some("secret-value"));
    assert_eq!(
        discord_webhook_url.as_deref(),
        Some("https://discord.com/api/webhooks/1/tok"),
        "the third destination column round-trips as plain text"
    );
}

#[tokio::test]
async fn deleting_the_definition_cascades_to_its_delivery_config() {
    let (_container, pool) = migrated_pool().await;
    let def = insert_definition(&pool).await;
    sqlx::query("INSERT INTO net_delivery_configs (definition_id) VALUES ($1)")
        .bind(def)
        .execute(&pool)
        .await
        .expect("insert config");

    // Account finalize is a single DELETE relying on every FK
    // to cascade — a RESTRICT here would block that. Prove the cascade.
    sqlx::query("DELETE FROM net_definitions WHERE id = $1")
        .bind(def)
        .execute(&pool)
        .await
        .expect("delete the definition (must cascade, not RESTRICT)");

    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_delivery_configs WHERE definition_id = $1")
            .bind(def)
            .fetch_one(&pool)
            .await
            .expect("count remaining config rows");
    assert_eq!(
        remaining, 0,
        "the config row is cascade-deleted with its net"
    );
}
