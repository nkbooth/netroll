// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves the `abuse_reports` and `audit_log` tables and the `accounts.disabled_at`
//! column exist with the right shape, against a real containerized Postgres.
//! The `audit_log` PII-free guarantee is asserted STRUCTURALLY: the table has no
//! column that could carry an email, token or secret.

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
        .expect("read columns")
        .into_iter()
        .map(|r| r.get::<String, _>("column_name"))
        .collect()
}

#[tokio::test]
async fn abuse_reports_body_is_mandatory_and_the_row_persists() {
    let (_c, pool) = migrated_pool().await;

    // A report with a body persists.
    sqlx::query("INSERT INTO abuse_reports (id, created_at, body) VALUES ($1, now(), $2)")
        .bind(Uuid::now_v7())
        .bind("spam in the discovery feed")
        .execute(&pool)
        .await
        .expect("a report with a body is recorded");

    // A report without a body is rejected (NOT NULL).
    let no_body =
        sqlx::query("INSERT INTO abuse_reports (id, created_at, body) VALUES ($1, now(), NULL)")
            .bind(Uuid::now_v7())
            .execute(&pool)
            .await;
    assert!(no_body.is_err(), "a report without a body is not a report");
}

/// Defense-in-depth: the column CHECK constraints
/// mirror the handler's `MAX_*_LEN` bounds exactly, so an unbounded blob can
/// never land in storage even from a caller that bypasses the HTTP handler.
#[tokio::test]
async fn abuse_reports_columns_reject_blank_and_oversized_values_at_the_db_level() {
    let (_c, pool) = migrated_pool().await;

    // An empty (but non-null) body is rejected — the handler already refuses
    // an empty/whitespace-only body; the CHECK enforces it structurally too.
    let empty_body =
        sqlx::query("INSERT INTO abuse_reports (id, created_at, body) VALUES ($1, now(), '')")
            .bind(Uuid::now_v7())
            .execute(&pool)
            .await;
    assert!(empty_body.is_err(), "an empty body is not a report");

    // A body over the 4000-char bound is rejected.
    let oversized_body =
        sqlx::query("INSERT INTO abuse_reports (id, created_at, body) VALUES ($1, now(), $2)")
            .bind(Uuid::now_v7())
            .bind("x".repeat(4_001))
            .execute(&pool)
            .await;
    assert!(
        oversized_body.is_err(),
        "a body over MAX_REPORT_BODY_LEN is rejected at the DB level"
    );

    // A reporter_contact over the 254-char bound is rejected.
    let oversized_contact = sqlx::query(
        "INSERT INTO abuse_reports (id, created_at, body, reporter_contact)
         VALUES ($1, now(), 'valid body', $2)",
    )
    .bind(Uuid::now_v7())
    .bind("x".repeat(255))
    .execute(&pool)
    .await;
    assert!(
        oversized_contact.is_err(),
        "a reporter_contact over MAX_REPORTER_CONTACT_LEN is rejected at the DB level"
    );

    // A context_url over the 2048-char bound is rejected.
    let oversized_context = sqlx::query(
        "INSERT INTO abuse_reports (id, created_at, body, context_url)
         VALUES ($1, now(), 'valid body', $2)",
    )
    .bind(Uuid::now_v7())
    .bind("x".repeat(2_049))
    .execute(&pool)
    .await;
    assert!(
        oversized_context.is_err(),
        "a context_url over MAX_CONTEXT_URL_LEN is rejected at the DB level"
    );
}

#[tokio::test]
async fn audit_log_has_only_pii_free_columns() {
    let (_c, pool) = migrated_pool().await;
    let cols = columns(&pool, "audit_log").await;

    // The exact generic shape — actor/action/target/time/meta, plus the two
    // typed context columns the admin review filters added (they record WHERE
    // an action happened, as opposed to what it acted on). Every column is an
    // id, a timestamp, a closed-vocabulary verb, or the bounded metadata blob:
    // the PII-free property is unchanged by the addition.
    let expected: HashSet<String> = [
        "id",
        "occurred_at",
        "actor_account_id",
        "action",
        "target_type",
        "target_id",
        "metadata",
        "context_session_id",
        "context_definition_id",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(cols, expected, "audit_log has exactly the generic columns");

    // PII/secret-free BY CONSTRUCTION: no column can carry an email/token/secret.
    for forbidden in ["email", "token", "password", "credential", "grid"] {
        assert!(
            !cols.contains(forbidden),
            "audit_log must have no {forbidden} column (PII/secret-free)"
        );
    }
}

#[tokio::test]
async fn accounts_disabled_at_is_distinct_from_deleted_at() {
    let (_c, pool) = migrated_pool().await;
    let cols = columns(&pool, "accounts").await;
    assert!(cols.contains("disabled_at"), "accounts has disabled_at");
    assert!(
        cols.contains("disabled_reason"),
        "accounts has disabled_reason"
    );
    // Both states coexist as SEPARATE columns — disabling is not deletion.
    assert!(
        cols.contains("deleted_at"),
        "deleted_at still exists, unmerged"
    );
}
