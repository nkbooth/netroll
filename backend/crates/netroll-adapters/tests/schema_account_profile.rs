// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the four `accounts` profile columns are nullable, unconstrained and
//! round-trip both ways (NULL → values → NULL) against a real containerized
//! Postgres. Unlike callsign there is no uniqueness: profile fields are
//! optional forever.

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

type ProfileRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn profile_columns(pool: &PgPool, id: Uuid) -> ProfileRow {
    let row =
        sqlx::query("SELECT display_name, location, grid, avatar_url FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("select profile columns");
    (
        row.get("display_name"),
        row.get("location"),
        row.get("grid"),
        row.get("avatar_url"),
    )
}

#[tokio::test]
async fn profile_columns_default_null_and_round_trip_values_and_back() {
    let (_container, pool) = migrated_pool().await;

    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, $2)")
        .bind(id)
        .bind("op@example.com")
        .execute(&pool)
        .await
        .expect("an account inserts with every profile column omitted");

    assert_eq!(
        profile_columns(&pool, id).await,
        (None, None, None, None),
        "all four profile columns default to NULL"
    );

    sqlx::query(
        "UPDATE accounts SET display_name = $2, location = $3, grid = $4, avatar_url = $5
         WHERE id = $1",
    )
    .bind(id)
    .bind("Maria")
    .bind("Hartford, CT")
    .bind("FN31pr")
    .bind("https://example.com/me.png")
    .execute(&pool)
    .await
    .expect("profile columns accept values");

    assert_eq!(
        profile_columns(&pool, id).await,
        (
            Some("Maria".to_owned()),
            Some("Hartford, CT".to_owned()),
            Some("FN31pr".to_owned()),
            Some("https://example.com/me.png".to_owned()),
        )
    );

    sqlx::query(
        "UPDATE accounts SET display_name = NULL, location = NULL, grid = NULL,
         avatar_url = NULL WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect("profile columns clear back to NULL — nullable, no constraints");

    assert_eq!(profile_columns(&pool, id).await, (None, None, None, None));
}

#[tokio::test]
async fn two_accounts_may_share_identical_profile_values() {
    let (_container, pool) = migrated_pool().await;

    for email in ["a@example.com", "b@example.com"] {
        sqlx::query(
            "INSERT INTO accounts (id, email, display_name, location, grid, avatar_url)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(email)
        .bind("Maria")
        .bind("Hartford, CT")
        .bind("FN31pr")
        .bind("https://example.com/me.png")
        .execute(&pool)
        .await
        .expect("identical profile values on two accounts must not collide — no uniqueness");
    }
}
