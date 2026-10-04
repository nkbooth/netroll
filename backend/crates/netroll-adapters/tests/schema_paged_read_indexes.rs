// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves `list_for_account_page` has an index matching its own `ORDER BY`,
//! served by it per the catalog, not query text. Only favorites is fenced:
//! the owned-nets read filters on the owners JOIN table, so no
//! `net_definitions` index can lead with the account — a `created_at` one was
//! tried and rejected, stealing the discovery `name` filter's plan for little gain.

use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use netroll_adapters::pg::favorites::FavoritesRepo;
use netroll_adapters::pg::net_definitions::NetDefinitionRepo;
use netroll_domain::net::connection::{NetConnection, NetConnectionKind, NetConnectionSet};
use netroll_domain::net::enums::{Band, Mode};
use netroll_domain::net::validation::{
    NetDefinitionFields, RawNetDefinition, parse_net_definition_fields,
};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use uuid::Uuid;

/// The index, the table it must sit on, and the column list its definition
/// must carry — the read's exact `ORDER BY`, leading key included.
const FAVORITES_INDEX: (&str, &str, &str) = (
    "idx_net_favorites_account_recent",
    "net_favorites",
    "(account_id, created_at DESC, net_definition_id DESC)",
);

/// A fresh migrated database: the container, a pool, and the URL a second pool
/// can be opened on.
async fn migrated_pool() -> (ContainerAsync<Postgres>, PgPool, String) {
    let container = Postgres::default()
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("resolve container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("resolve mapped postgres port");

    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let pool = PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("connect to containerized postgres");

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations against fresh database");

    (container, pool, url)
}

/// A second pool over the SAME database whose ONE session has sequential scans
/// AND sorts disabled. On a test's near-empty table the planner would
/// otherwise scan-and-sort for any `ORDER BY … LIMIT`, indexable or not, and
/// the counter read below could not tell an index that matches the read's
/// order from one that does not. With both penalised, the only cheap plan is
/// an index walked in the read's own order — which exists exactly when the
/// index's columns and directions match the `ORDER BY`.
///
/// Exactly one connection, on purpose: a backend only ships its scan counters
/// to the statistics collector when it finishes a LATER command, so the poll
/// below must run on the very connection that ran the query.
async fn ordered_plan_pool(url: &str) -> PgPool {
    let options = PgConnectOptions::from_str(url)
        .expect("parse database url")
        .options([("enable_seqscan", "off"), ("enable_sort", "off")]);
    PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect with sequential scans and sorts disabled")
}

/// Whether `index` reports at least one scan, polled for a few seconds through
/// the SAME single-connection pool that issued the query — the statistics
/// collector publishes counters asynchronously.
async fn index_was_scanned(pool: &PgPool, index: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let scans: i64 =
            sqlx::query_scalar("SELECT idx_scan FROM pg_stat_user_indexes WHERE indexrelname = $1")
                .bind(index)
                .fetch_one(pool)
                .await
                .expect("read pg_stat_user_indexes");
        if scans > 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64
}

async fn seed_account(pool: &PgPool, email: &str) -> Uuid {
    netroll_adapters::pg::accounts::AccountRepo::new(pool.clone())
        .create_verified_and_attach(email, now_millis())
        .await
        .expect("seed account")
        .id
}

fn sample_connections() -> NetConnectionSet {
    NetConnectionSet::new(vec![NetConnection {
        id: Uuid::now_v7(),
        position: 0,
        kind: NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band: Band::TwentyMeters,
            mode: Mode::Ssb,
        },
    }])
    .expect("one HF connection is a valid set")
}

fn sample_fields() -> NetDefinitionFields {
    parse_net_definition_fields(RawNetDefinition {
        title: Some("Index Fixture Net".to_owned()),
        description: None,
        country: None,
        state: None,
        grid: None,
        net_category: Some("traffic".to_owned()),
        net_type: Some("open".to_owned()),
        expected_duration: None,
        visibility: None,
    })
    .expect("valid fixture fields")
}

#[tokio::test]
async fn the_favorites_page_has_an_index_matching_its_order() {
    let (_container, pool, _) = migrated_pool().await;
    let (index, table, columns) = FAVORITES_INDEX;

    let definition: Option<String> = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes WHERE indexname = $1 AND tablename = $2",
    )
    .bind(index)
    .bind(table)
    .fetch_optional(&pool)
    .await
    .expect("query pg_indexes");

    let definition =
        definition.unwrap_or_else(|| panic!("{index} must exist on {table} after migrations"));
    assert!(
        definition.ends_with(columns),
        "{index} must be composite on the read's exact ORDER BY `{columns}` — its definition \
         is `{definition}`"
    );
}

#[tokio::test]
async fn the_favorites_page_is_served_by_its_index() {
    let (_container, pool, url) = migrated_pool().await;
    let account = seed_account(&pool, "favorites-index@example.com").await;
    let owner = seed_account(&pool, "owner@example.com").await;
    let definitions = NetDefinitionRepo::new(pool.clone());
    let favorites = FavoritesRepo::new(pool.clone());
    let base = now_millis();
    for i in 0..3_u64 {
        let net = definitions
            .create(
                &sample_fields(),
                &sample_connections(),
                owner,
                &format!("tok-fav-idx-{i}"),
                base + i,
            )
            .await
            .expect("create net")
            .id;
        favorites
            .add(account, net, base + i)
            .await
            .expect("favorite");
    }

    let scan_pool = ordered_plan_pool(&url).await;
    let page = FavoritesRepo::new(scan_pool.clone())
        .list_for_account_page(account, 2, None)
        .await
        .expect("favorites page");
    assert_eq!(page.rows.len(), 2, "the fixture pages");

    assert!(
        index_was_scanned(&scan_pool, FAVORITES_INDEX.0).await,
        "list_for_account_page must be served by {} — an index whose columns or directions \
         differ from `{}` cannot feed the LIMIT in order and is left untouched",
        FAVORITES_INDEX.0,
        FAVORITES_INDEX.2
    );
}
