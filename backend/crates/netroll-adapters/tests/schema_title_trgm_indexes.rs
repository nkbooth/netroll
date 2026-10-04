// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the two `pg_trgm` GIN indexes the three `ILIKE` title searches lean
//! on exist — one on `net_definitions.title`, serving both the discovery `q`
//! filter and the admin net search, and one on the frozen `net_sessions`
//! snapshot title. Asserts the catalog, never any query text.

use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use netroll_adapters::pg::admin_search::AdminSearchRepo;
use netroll_adapters::pg::discovery::DiscoveryRepo;
use netroll_domain::net::discovery::{DiscoveryFilters, DiscoveryQuery};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

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
/// disabled. On the near-empty tables of a test the planner would otherwise
/// scan for ANY predicate, indexable or not, and the counter read below could
/// not tell the two query forms apart. With scans disabled, a predicate the
/// index can serve is served by it and one it cannot serve still falls back to
/// the (penalised) scan — exactly the line between `title ILIKE $1`
/// and `lower(title) ILIKE $1`.
///
/// Exactly one connection, on purpose: a backend only ships its scan counters
/// to the statistics collector when it finishes a LATER command, so the poll
/// below must run on the very connection that ran the query, or the counters
/// sit unreported in an idle backend for as long as the test waits.
async fn no_seqscan_pool(url: &str) -> PgPool {
    let options = PgConnectOptions::from_str(url)
        .expect("parse database url")
        .options([("enable_seqscan", "off")]);
    PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect with sequential scans disabled")
}

/// Whether `pg_stat_user_indexes.idx_scan` for `index` becomes positive within
/// a few seconds, read through the SAME single-connection pool that issued the
/// query. Polled rather than read once: the statistics collector publishes
/// counters asynchronously — at least half a second behind on the PG 11 the
/// test containers run — and each poll is itself the later command that makes
/// the backend report what it counted.
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

/// The two indexes, each paired with the table it must sit on. Two and not
/// three: the discovery `q` filter and the admin net search both test
/// `net_definitions.title`, so one index serves both sites.
const TRIGRAM_INDEXES: [(&str, &str); 2] = [
    ("idx_net_definitions_title_trgm", "net_definitions"),
    ("idx_net_sessions_snapshot_title_trgm", "net_sessions"),
];

#[tokio::test]
async fn both_title_trigram_gin_indexes_exist_after_migrations() {
    let (_container, pool, _) = migrated_pool().await;

    for (index, table) in TRIGRAM_INDEXES {
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
        let lowered = definition.to_lowercase();
        assert!(
            lowered.contains("using gin") && lowered.contains("gin_trgm_ops"),
            "{index} must be a GIN index over gin_trgm_ops — a btree here serves no ILIKE \
             substring search; its definition is `{definition}`"
        );
    }
}

// The index-FORM fences (mutation-proved under). Existence above says
// the index is there; these say the three production predicates are written in
// the shape it serves. Re-wrapping a column in `lower()` leaves every existence
// and escaping test green while the read goes back to a full scan — the only
// surface that notices is the catalog's scan counter on the index itself.

#[tokio::test]
async fn both_admin_title_searches_are_served_by_their_trigram_index() {
    let (_container, _pool, url) = migrated_pool().await;
    let scan_pool = no_seqscan_pool(&url).await;
    let repo = AdminSearchRepo::new(scan_pool.clone());

    repo.net_definitions_by_title("ragchew", 20)
        .await
        .expect("net definition search");
    repo.net_sessions_by_snapshot_title("ragchew", 20)
        .await
        .expect("net session search");

    assert!(
        index_was_scanned(&scan_pool, "idx_net_definitions_title_trgm").await,
        "net_definitions_by_title must be served by idx_net_definitions_title_trgm — a \
         `lower(title)` wrapper or a non-ILIKE form leaves the index untouched"
    );
    assert!(
        index_was_scanned(&scan_pool, "idx_net_sessions_snapshot_title_trgm").await,
        "net_sessions_by_snapshot_title must be served by idx_net_sessions_snapshot_title_trgm \
         — the expression must match the index's exactly, with no wrapper"
    );
}

#[tokio::test]
async fn the_discovery_name_filter_is_served_by_the_title_trigram_index() {
    let (_container, _pool, url) = migrated_pool().await;
    let scan_pool = no_seqscan_pool(&url).await;
    let repo = DiscoveryRepo::new(scan_pool.clone());
    let query = DiscoveryQuery {
        filters: DiscoveryFilters {
            name: Some("ragchew".to_owned()),
            ..DiscoveryFilters::default()
        },
        ..DiscoveryQuery::default()
    };

    repo.list_discovery_upcoming(&query, now_millis(), 100)
        .await
        .expect("discovery read");

    assert!(
        index_was_scanned(&scan_pool, "idx_net_definitions_title_trgm").await,
        "list_discovery_upcoming's `name` predicate must be served by \
         idx_net_definitions_title_trgm — a `lower(d.title)` wrapper leaves it untouched"
    );
}
