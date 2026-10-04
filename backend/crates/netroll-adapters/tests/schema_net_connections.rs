// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves `net_connections`' structural constraints, the cascade, the review
//! aggregate and the DMR network decode against a real containerized Postgres.
//! The backfill replays that used to live here are RETIRED: that SQL reads flat
//! columns a later migration drops, and a from-scratch replay still runs it
//! before the drop, in order.

use netroll_adapters::pg::net_connections::load_for_definition;
use netroll_domain::net::connection::NetConnectionKind;
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

/// Inserts a bare net definition row — deliberately with NO connection rows,
/// because every test here inserts exactly the connection rows it is about.
/// The row itself carries no connection fact to seed.
async fn insert_definition(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO net_definitions (id, title, net_category, net_type, link_token)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind("Sunday Traffic Net")
    .bind("traffic")
    .bind("open")
    .bind(Uuid::now_v7().simple().to_string())
    .execute(pool)
    .await
    .expect("insert net definition");
    id
}

async fn insert_connection(
    pool: &PgPool,
    definition_id: Uuid,
    position: i32,
    kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO net_connections (id, definition_id, position, kind)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(definition_id)
    .bind(position)
    .bind(kind)
    .execute(pool)
    .await
    .map(|_| ())
}

// --- Two structural constraints live in the schema ---------------------

#[tokio::test]
async fn two_connections_cannot_share_a_position_on_one_definition() {
    let (_container, pool) = migrated_pool().await;
    let definition_id = insert_definition(&pool).await;

    insert_connection(&pool, definition_id, 1, "echolink")
        .await
        .expect("the first connection at position 1 is accepted");
    let duplicate = insert_connection(&pool, definition_id, 1, "allstar").await;

    assert!(
        duplicate.is_err(),
        "a second connection at the same (definition_id, position) must be refused — otherwise \
         the reader silently takes whichever row the planner returned first"
    );
}

#[tokio::test]
async fn only_one_connection_per_definition_can_hold_the_export_position() {
    let (_container, pool) = migrated_pool().await;
    let definition_id = insert_definition(&pool).await;

    insert_connection(&pool, definition_id, 0, "hf")
        .await
        .expect("the first connection at position 0 is accepted");
    // Which constraint refuses this is deliberately NOT asserted, and cannot be:
    // the partial unique index on (definition_id) WHERE position = 0 is IMPLIED
    // by UNIQUE (definition_id, position), so no INSERT can distinguish them.
    // What is asserted is the guarantee the export depends on.
    let second_at_zero = insert_connection(&pool, definition_id, 0, "hf").await;

    assert!(
        second_at_zero.is_err(),
        "two rows both claiming the export position must be refused"
    );
}

#[tokio::test]
async fn the_export_position_guarantee_is_named_in_its_own_index() {
    let (_container, pool) = migrated_pool().await;

    let predicate: Option<String> = sqlx::query_scalar(
        "SELECT pg_get_expr(i.indpred, i.indrelid)
           FROM pg_index i
           JOIN pg_class c ON c.oid = i.indexrelid
          WHERE c.relname = 'idx_net_connections_export_position' AND i.indisunique",
    )
    .fetch_optional(&pool)
    .await
    .expect("query pg_index")
    .flatten();

    // The composite UNIQUE already implies this index, so it can never red on a
    // write. It is kept so a later migration that loosens the composite
    // constraint cannot take the export-position guarantee with it silently,
    // and this is the only check that it is still there to do that job.
    assert!(
        predicate.is_some_and(|p| p.contains("position") && p.contains('0')),
        "a unique index on (definition_id) partial on position = 0 must exist"
    );
}

#[tokio::test]
async fn a_connection_does_not_outlive_its_definition() {
    let (_container, pool) = migrated_pool().await;
    let definition_id = insert_definition(&pool).await;
    // The definition is inserted AFTER the migrations have run, so the backfill
    // never saw it and it has no connection rows. Without this insert the count
    // below is zero throughout and the test passes under RESTRICT, under NO
    // ACTION and with no foreign key at all.
    insert_connection(&pool, definition_id, 0, "hf")
        .await
        .expect("the definition has a connection to outlive it");
    let before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_connections WHERE definition_id = $1")
            .bind(definition_id)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(before, 1, "the cascade has something to act on");

    sqlx::query("DELETE FROM net_definitions WHERE id = $1")
        .bind(definition_id)
        .execute(&pool)
        .await
        .expect("a definition with connections can still be hard-deleted");

    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_connections WHERE definition_id = $1")
            .bind(definition_id)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(
        remaining, 0,
        "a connection is configuration, not history — it cascades with its definition"
    );
}

// --- The review aggregate counts one row per normalised label ---------

#[tokio::test]
async fn the_review_aggregate_counts_one_row_per_normalised_label() {
    let (_container, pool) = migrated_pool().await;
    let definition_id = insert_definition(&pool).await;

    for (position, label) in ["Wires-X", " wires  x ", "WIRES-X"].iter().enumerate() {
        sqlx::query(
            "INSERT INTO net_connections (id, definition_id, position, kind, label)
             VALUES ($1, $2, $3, 'other', $4)",
        )
        .bind(Uuid::now_v7())
        .bind(definition_id)
        .bind(position as i32)
        .bind(label)
        .execute(&pool)
        .await
        .expect("insert an `other` connection");
    }

    // Verbatim the aggregate the backfill migration's header records, which is
    // the instrument the item-9 review trigger is answered with. The SQL twin of
    // the domain's `count_key_of`.
    let rows = sqlx::query(
        "SELECT btrim(regexp_replace(lower(label), '[^[:alnum:]]+', ' ', 'g')) AS count_key,
                count(*)
           FROM net_connections
          WHERE kind = 'other'
          GROUP BY 1
          ORDER BY count(*) DESC",
    )
    .fetch_all(&pool)
    .await
    .expect("run the review aggregate");

    assert_eq!(
        rows.len(),
        1,
        "three spellings of one label are one answer to `is the kind set cut wrong`; a \
         GROUP BY on the raw column makes them three"
    );
    assert_eq!(rows[0].get::<String, _>("count_key"), "wires x");
    assert_eq!(rows[0].get::<i64, _>("count"), 3);
}

// --- A DMR connection names its network ------------------------

/// Inserts one `dmr` row with exactly the payload columns the caller supplies.
///
/// Hand-written rather than routed through `replace_connections` because the
/// point is what a STORED row decodes to — including shapes the domain would
/// never write, which is how the degrade path is reached at all.
async fn insert_dmr_connection(
    pool: &PgPool,
    definition_id: Uuid,
    position: i32,
    talkgroup: Option<&str>,
    network: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO net_connections (id, definition_id, position, kind, talkgroup, network)
         VALUES ($1, $2, $3, 'dmr', $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(definition_id)
    .bind(position)
    .bind(talkgroup)
    .bind(network)
    .execute(pool)
    .await
    .map(|_| ())
}

/// The one connection `definition_id` holds, decoded by the adapter.
async fn only_connection(pool: &PgPool, definition_id: Uuid) -> NetConnectionKind {
    let set = load_for_definition(pool, definition_id)
        .await
        .expect("load the definition's connections")
        .expect("the definition has a connection set");
    let connections = set.connections();
    assert_eq!(connections.len(), 1, "the fixture inserts exactly one row");
    connections[0].kind.clone()
}

#[tokio::test]
async fn a_stored_dmr_row_carrying_a_network_decodes_with_it() {
    let (_container, pool) = migrated_pool().await;
    let definition_id = insert_definition(&pool).await;
    insert_dmr_connection(&pool, definition_id, 0, Some("3100"), Some("TGIF"))
        .await
        .expect("insert a dmr row carrying a network");

    assert_eq!(
        only_connection(&pool, definition_id).await,
        NetConnectionKind::Dmr {
            talkgroup: "3100".to_owned(),
            network: Some("TGIF".to_owned()),
        },
        "a stored network must survive the decode, not merely the column"
    );
}

#[tokio::test]
async fn a_stored_dmr_row_with_no_network_is_still_a_dmr_connection() {
    let (_container, pool) = migrated_pool().await;
    let definition_id = insert_definition(&pool).await;
    insert_dmr_connection(&pool, definition_id, 0, Some("3100"), None)
        .await
        .expect("insert a dmr row with no network");

    let kind = only_connection(&pool, definition_id).await;
    // The KIND is asserted, not merely that the load succeeded: a degrade to
    // `other` loads too, and `row.network.clone()?` would turn every DMR
    // connection on the instance into `unclassified-dmr` residue.
    assert_eq!(kind.as_str(), "dmr");
    assert_eq!(
        kind,
        NetConnectionKind::Dmr {
            talkgroup: "3100".to_owned(),
            network: None,
        }
    );
}

#[tokio::test]
async fn a_dmr_row_the_domain_refuses_keeps_its_network_in_the_residue() {
    let (_container, pool) = migrated_pool().await;
    let definition_id = insert_definition(&pool).await;
    // A network with no talkgroup: the domain has no such DMR connection, so
    // this row degrades — and the degrade must not be where the network is lost.
    insert_dmr_connection(&pool, definition_id, 0, None, Some("FreeDMR"))
        .await
        .expect("insert a domain-forbidden dmr row");

    match only_connection(&pool, definition_id).await {
        NetConnectionKind::Other { label, detail } => {
            assert_eq!(label, "unclassified-dmr");
            assert!(
                detail
                    .as_deref()
                    .expect("the residue keeps the row's values")
                    .contains("network=FreeDMR"),
                "the degrade must not be where an operator's network is thrown away"
            );
        }
        other => panic!("a talkgroup-less dmr row must degrade, got {other:?}"),
    }
}
