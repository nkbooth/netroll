// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the audit log's typed context columns, filter indexes and account
//! prefix-search indexes exist with the right shape, against a real
//! containerized Postgres. These assert PHYSICAL schema, never query text: an
//! index the planner can use is what the filter surface depends on, and a doc
//! comment claiming one exists is not evidence.

use std::collections::HashSet;

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

async fn columns(pool: &PgPool, table: &str) -> HashSet<String> {
    sqlx::query("SELECT column_name FROM information_schema.columns WHERE table_name = $1")
        .bind(table)
        .fetch_all(pool)
        .await
        .expect("read information_schema.columns")
        .into_iter()
        .map(|r| r.get::<String, _>("column_name"))
        .collect()
}

async fn index_defs(pool: &PgPool, table: &str) -> Vec<(String, String)> {
    sqlx::query("SELECT indexname, indexdef FROM pg_indexes WHERE tablename = $1")
        .bind(table)
        .fetch_all(pool)
        .await
        .expect("read pg_indexes")
        .into_iter()
        .map(|r| {
            (
                r.get::<String, _>("indexname"),
                r.get::<String, _>("indexdef"),
            )
        })
        .collect()
}

fn find<'a>(defs: &'a [(String, String)], name: &str) -> &'a str {
    defs.iter()
        .find(|(n, _)| n == name)
        .map(|(_, d)| d.as_str())
        .unwrap_or_else(|| panic!("index {name} exists; found {defs:?}"))
}

#[tokio::test]
async fn audit_log_carries_typed_context_columns() {
    // The linkage to a session/net must be a real uuid column, not a text
    // rendering inside jsonb — a text match agrees with a uuid only by accident
    // of formatting, and would fail silently (fewer rows, no error).
    let (_c, pool) = migrated_pool().await;
    let cols = columns(&pool, "audit_log").await;

    assert!(cols.contains("context_session_id"));
    assert!(cols.contains("context_definition_id"));

    let types: Vec<String> = sqlx::query(
        "SELECT data_type FROM information_schema.columns
         WHERE table_name = 'audit_log'
           AND column_name IN ('context_session_id', 'context_definition_id')",
    )
    .fetch_all(&pool)
    .await
    .expect("read column types")
    .into_iter()
    .map(|r| r.get::<String, _>("data_type"))
    .collect();
    assert_eq!(types.len(), 2);
    assert!(
        types.iter().all(|t| t == "uuid"),
        "context columns are uuid, not text: {types:?}"
    );
}

#[tokio::test]
async fn the_context_columns_stay_pii_free_and_nullable() {
    // Most rows (sign-ins, admin reads) have no context at all — a NOT NULL
    // would force a sentinel, and the partial indexes assume real NULLs.
    let (_c, pool) = migrated_pool().await;
    let nullable: Vec<String> = sqlx::query(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'audit_log'
           AND column_name IN ('context_session_id', 'context_definition_id')",
    )
    .fetch_all(&pool)
    .await
    .expect("read nullability")
    .into_iter()
    .map(|r| r.get::<String, _>("is_nullable"))
    .collect();
    assert!(nullable.iter().all(|n| n == "YES"), "{nullable:?}");

    // The whole-table PII guarantee still holds: no column can carry an
    // email/token/secret. Adding two uuid columns must not have weakened it.
    let cols = columns(&pool, "audit_log").await;
    for forbidden in ["email", "token", "password", "secret", "callsign", "grid"] {
        assert!(
            !cols.iter().any(|c| c.contains(forbidden)),
            "audit_log has no {forbidden}-bearing column; got {cols:?}"
        );
    }
}

#[tokio::test]
async fn the_backfill_lifts_an_existing_metadata_session_id_into_the_typed_column() {
    // Rows written before this migration carry the session only in
    // metadata->>'netSessionId'. If the backfill missed them they would be
    // invisible to the new filter — the exact silent-underreporting failure the
    // typed column exists to prevent.
    let (_c, pool) = migrated_pool().await;
    let actor = Uuid::now_v7();
    let session = Uuid::now_v7();

    // Simulate a pre-migration row, then re-run the backfill statement.
    sqlx::query(
        "INSERT INTO audit_log (id, occurred_at, actor_account_id, action, target_type, target_id, metadata)
         VALUES ($1, now(), $2, 'role-granted', 'account', $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(actor)
    .bind(Uuid::now_v7())
    .bind(serde_json::json!({ "role": "logger", "netSessionId": session }))
    .execute(&pool)
    .await
    .expect("insert legacy-shaped row");

    sqlx::query(
        "UPDATE audit_log
            SET context_session_id = NULLIF(metadata->>'netSessionId', '')::uuid
          WHERE metadata ? 'netSessionId' AND context_session_id IS NULL",
    )
    .execute(&pool)
    .await
    .expect("re-run backfill");

    let lifted: Option<Uuid> = sqlx::query_scalar(
        "SELECT context_session_id FROM audit_log WHERE action = 'role-granted'",
    )
    .fetch_one(&pool)
    .await
    .expect("read back");
    assert_eq!(lifted, Some(session));
}

#[tokio::test]
async fn every_audit_filter_has_a_supporting_ordered_index() {
    // Each filter walks the index in the review surface's own sort order so the
    // LIMIT terminates early. An index missing the DESC ordering would still be
    // "present" while forcing a full sort — assert the ordering, not just the name.
    let (_c, pool) = migrated_pool().await;
    let defs = index_defs(&pool, "audit_log").await;

    for (name, key) in [
        ("idx_audit_log_actor_recent", "actor_account_id"),
        ("idx_audit_log_target_recent", "target_id"),
        ("idx_audit_log_context_session_recent", "context_session_id"),
        (
            "idx_audit_log_context_definition_recent",
            "context_definition_id",
        ),
    ] {
        let def = find(&defs, name);
        assert!(def.contains(key), "{name} leads on {key}: {def}");
        assert!(
            def.contains("occurred_at DESC") && def.contains("id DESC"),
            "{name} is ordered to match the review surface's ORDER BY: {def}"
        );
    }
}

#[tokio::test]
async fn the_target_and_context_indexes_are_partial() {
    // Most rows have no target/context; indexing their NULLs would bloat the
    // index with rows no filter can ever select.
    let (_c, pool) = migrated_pool().await;
    let defs = index_defs(&pool, "audit_log").await;

    for name in [
        "idx_audit_log_target_recent",
        "idx_audit_log_context_session_recent",
        "idx_audit_log_context_definition_recent",
    ] {
        let def = find(&defs, name);
        assert!(def.contains("IS NOT NULL"), "{name} is partial: {def}");
    }
}

#[tokio::test]
async fn there_is_no_index_on_the_low_selectivity_action_column() {
    // ~16 closed verbs: `action` is a post-filter, not a driving index. An index
    // here would be dead weight the planner ignores.
    let (_c, pool) = migrated_pool().await;
    let defs = index_defs(&pool, "audit_log").await;
    assert!(
        !defs
            .iter()
            .any(|(n, d)| n.contains("action") || d.contains("(action)")),
        "no action index: {defs:?}"
    );
}

#[tokio::test]
async fn account_prefix_search_indexes_use_the_pattern_operator_class() {
    // `lower(col) LIKE 'x%'` does NOT use a plain btree on lower(col) under a
    // non-C collation. Without text_pattern_ops the bounded prefix search
    // silently degrades to a sequential scan — trading the anti-harvesting
    // fence for a full table read.
    let (_c, pool) = migrated_pool().await;
    let defs = index_defs(&pool, "accounts").await;

    for (name, key) in [
        ("idx_accounts_callsign_prefix", "callsign"),
        ("idx_accounts_display_name_prefix", "display_name"),
    ] {
        let def = find(&defs, name);
        assert!(def.contains("lower"), "{name} indexes lower({key}): {def}");
        assert!(
            def.contains("text_pattern_ops"),
            "{name} uses text_pattern_ops so LIKE 'x%' can use it: {def}"
        );
    }
}

#[tokio::test]
async fn email_gets_no_prefix_index_so_it_stays_exact_match_only() {
    // The anti-harvesting fence: callsign and display name are public radio
    // data and may be prefix-searched; an address must not be. No pattern-ops
    // index on email is the structural half of that decision.
    let (_c, pool) = migrated_pool().await;
    let defs = index_defs(&pool, "accounts").await;
    assert!(
        !defs
            .iter()
            .any(|(_, d)| d.contains("email") && d.contains("text_pattern_ops")),
        "no prefix-search index on email: {defs:?}"
    );
}
