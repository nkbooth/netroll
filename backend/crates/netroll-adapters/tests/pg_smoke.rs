// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Smoke test for the testcontainers-backed Postgres harness: proves
//! integration tests run against a REAL Postgres, not mocked SQL. Establishes
//! the harness for the event store; no schema yet.

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
async fn postgres_round_trip_query() {
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

    let row = sqlx::query("SELECT 1 + 1")
        .fetch_one(&pool)
        .await
        .expect("execute round-trip query");
    let value: i32 = row.get(0);
    assert_eq!(value, 2);
}
