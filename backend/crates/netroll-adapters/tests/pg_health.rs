// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The liveness ping answers from the database itself: a pool that cannot
//! reach Postgres must fail the ping, and a live one must pass it.

use netroll_adapters::pg::health::HealthRepo;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
async fn ping_succeeds_against_a_live_database() {
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

    HealthRepo::new(pool)
        .ping()
        .await
        .expect("a live database answers the ping");
}

#[tokio::test]
async fn ping_fails_on_a_closed_pool() {
    let doomed = PgPoolOptions::new()
        .connect_lazy("postgres://x:x@127.0.0.1:1/x")
        .expect("a lazy pool parses its url without connecting");
    doomed.close().await;

    let outcome = HealthRepo::new(doomed).ping().await;

    assert!(
        outcome.is_err(),
        "a pool that cannot reach the database must fail the ping"
    );
}
