// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Proves, against a real containerized Postgres, that
//! `accounts.deleted_at` round-trips NULL and a timestamp, that deleting an
//! account cascades to all four child tables, and that `magic_link_tokens`
//! (email-keyed, uncascaded) survives.

use chrono::{DateTime, Utc};
use netroll_adapters::pg::accounts::AccountRepo;
use netroll_adapters::pg::favorites::FavoritesRepo;
use netroll_adapters::pg::net_definitions::{AddOwnerOutcome, NetDefinitionRepo};
use netroll_adapters::pg::net_sessions::{NetSessionRepo, StartOutcome};
use netroll_domain::net::connection::{NetConnection, NetConnectionKind, NetConnectionSet};
use netroll_domain::net::enums::{Band, Mode};
use netroll_domain::net::validation::{
    NetDefinitionFields, RawNetDefinition, parse_net_definition_fields,
};
use sqlx::PgPool;
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

#[tokio::test]
async fn deleted_at_defaults_null_and_round_trips_a_timestamp() {
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;

    // A fresh account is live: deleted_at is NULL.
    let fresh: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .expect("read deleted_at");
    assert!(fresh.is_none(), "a fresh account has no deleted_at");

    // Marking it pending stores and round-trips the instant.
    let marked = Utc::now();
    sqlx::query("UPDATE accounts SET deleted_at = $2 WHERE id = $1")
        .bind(account_id)
        .bind(marked)
        .execute(&pool)
        .await
        .expect("mark pending");
    let stored: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .expect("read deleted_at");
    assert_eq!(
        stored.map(|t| t.timestamp_millis()),
        Some(marked.timestamp_millis()),
        "deleted_at round-trips the stored instant"
    );
}

#[tokio::test]
async fn deleting_an_account_cascades_to_every_child_table() {
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;

    // One child row in EACH of the four cascade tables.
    sqlx::query(
        "INSERT INTO auth_methods (id, account_id, kind, last_used_at)
         VALUES ($1, $2, 'magic-link', now())",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .execute(&pool)
    .await
    .expect("seed auth_method");

    sqlx::query(
        "INSERT INTO sessions (id, account_id, token_hash, last_seen_at, absolute_expires_at)
         VALUES ($1, $2, $3, now(), now() + interval '30 days')",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(&b"session-hash-cascade"[..])
    .execute(&pool)
    .await
    .expect("seed session");

    sqlx::query(
        "INSERT INTO account_consents (id, account_id, terms_version, consented_at)
         VALUES ($1, $2, '2026-07-15', now())",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .execute(&pool)
    .await
    .expect("seed consent");

    sqlx::query(
        "INSERT INTO email_change_tokens (id, account_id, new_email, token_hash, expires_at)
         VALUES ($1, $2, 'new@example.com', $3, now() + interval '15 minutes')",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(&b"change-hash-cascade"[..])
    .execute(&pool)
    .await
    .expect("seed email-change token");

    // The single hard delete must succeed — a NO ACTION FK would block it.
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("hard delete cascades rather than blocking");

    // Static SQL per table (sqlx 0.9 rejects dynamic query strings).
    for (table, remaining) in [
        (
            "auth_methods",
            count(
                &pool,
                "SELECT count(*) FROM auth_methods WHERE account_id = $1",
                account_id,
            )
            .await,
        ),
        (
            "sessions",
            count(
                &pool,
                "SELECT count(*) FROM sessions WHERE account_id = $1",
                account_id,
            )
            .await,
        ),
        (
            "account_consents",
            count(
                &pool,
                "SELECT count(*) FROM account_consents WHERE account_id = $1",
                account_id,
            )
            .await,
        ),
        (
            "email_change_tokens",
            count(
                &pool,
                "SELECT count(*) FROM email_change_tokens WHERE account_id = $1",
                account_id,
            )
            .await,
        ),
    ] {
        assert_eq!(
            remaining, 0,
            "{table} rows must cascade-delete with the account"
        );
    }
}

async fn count(pool: &PgPool, sql: &'static str, account_id: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("count child rows")
}

#[tokio::test]
async fn magic_link_tokens_are_not_cascaded_they_are_email_keyed() {
    // magic_link_tokens has NO account FK — it is keyed on the (normalized)
    // email, because the account may not exist when a link is issued. When an
    // account is finalized its email frees; a leftover valid magic link is
    // harmless (consuming it mints a fresh empty account for that email, the
    // intended outcome). So the delete must leave magic_link_tokens untouched.
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;

    sqlx::query(
        "INSERT INTO magic_link_tokens (id, email, token_hash, expires_at)
         VALUES ($1, 'op@example.com', $2, now() + interval '15 minutes')",
    )
    .bind(Uuid::now_v7())
    .bind(&b"magic-hash-survives"[..])
    .execute(&pool)
    .await
    .expect("seed magic-link token");

    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("delete account");

    let surviving: i64 =
        sqlx::query_scalar("SELECT count(*) FROM magic_link_tokens WHERE email = 'op@example.com'")
            .fetch_one(&pool)
            .await
            .expect("count magic-link tokens");
    assert_eq!(
        surviving, 1,
        "an email-keyed magic link survives the account delete (freed email is reusable)"
    );
}

/// A minimal valid net-definition write shape for the cascade test.
/// The one connection every fixture net is born with: a
/// definition carries no connection fact of its own, so `create` takes the set
/// as an argument.
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
    .expect("one connection is a valid set")
}

fn cascade_fields(title: &str) -> NetDefinitionFields {
    parse_net_definition_fields(RawNetDefinition {
        title: Some(title.to_owned()),
        description: None,
        country: Some("USA".to_owned()),
        state: Some("CT".to_owned()),
        grid: None,
        net_category: Some("traffic".to_owned()),
        net_type: Some("open".to_owned()),
        expected_duration: Some("90".to_owned()),
        visibility: None,
    })
    .expect("valid fields")
}

/// The comprehensive, CURRENT cross-cutting proof
/// that an account hard-delete clears every personal-data table that exists
/// today (the four original tables PLUS the four added since — `qrz_credentials`,
/// `net_favorites`, `net_definition_owners`, `net_session_roles`), while the
/// deliberately non-FK historical/operational records (`audit_log`,
/// `abuse_reports`, `session_events`) and the callsign-keyed `net_definition_roster`
/// survive untouched — AND the ownership consequences (co-owned net
/// survives with the other owner; solely-owned net is archived, not deleted).
/// This changes NO production code; a failure here is a real regression.
#[tokio::test]
async fn account_finalize_cascades_every_personal_table_and_survivors_remain() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let defs = NetDefinitionRepo::new(pool.clone());
    let sessions = NetSessionRepo::new(pool.clone());

    let now: u64 = 1_800_000_000_000;

    // The account under deletion, plus a co-owner that must survive.
    let account = accounts
        .create_verified_and_attach("delete-me@example.com", now)
        .await
        .expect("seed account")
        .id;
    let keeper = accounts
        .create_verified_and_attach("keeper@example.com", now)
        .await
        .expect("seed keeper")
        .id;

    // --- The four ORIGINAL cascade tables. `create_verified_and_attach`
    // already seeded the account's `auth_methods` (magic-link) row, so only
    // the other three are seeded raw here. ---
    sqlx::query(
        "INSERT INTO sessions (id, account_id, token_hash, last_seen_at, absolute_expires_at)
         VALUES ($1, $2, $3, now(), now() + interval '30 days')",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .bind(&b"session-hash-full-cascade"[..])
    .execute(&pool)
    .await
    .expect("seed session");
    sqlx::query(
        "INSERT INTO account_consents (id, account_id, terms_version, consented_at)
         VALUES ($1, $2, '2026-07-15', now())",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .execute(&pool)
    .await
    .expect("seed consent");
    sqlx::query(
        "INSERT INTO email_change_tokens (id, account_id, new_email, token_hash, expires_at)
         VALUES ($1, $2, 'new@example.com', $3, now() + interval '15 minutes')",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .bind(&b"change-hash-full-cascade"[..])
    .execute(&pool)
    .await
    .expect("seed email-change token");

    // --- qrz_credentials (added later). ---
    sqlx::query(
        "INSERT INTO qrz_credentials
             (account_id, wrapped_dek, dek_nonce, credential_ciphertext, credential_nonce)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(account)
    .bind(&b"wrapped"[..])
    .bind(&b"nonce1"[..])
    .bind(&b"cipher"[..])
    .bind(&b"nonce2"[..])
    .execute(&pool)
    .await
    .expect("seed qrz credentials");

    // --- net_definition_owners: a co-owned net (survives) and a solely-owned
    // net (archived). Both give the account an owner row that must cascade. ---
    let co_net = defs
        .create(
            &cascade_fields("Co-owned Net"),
            &sample_connections(),
            account,
            "tok-co",
            now,
        )
        .await
        .expect("create co-owned net");
    assert!(matches!(
        defs.add_owner(co_net.id, keeper, now, 10)
            .await
            .expect("add keeper as co-owner"),
        AddOwnerOutcome::Added
    ));
    let sole_net = defs
        .create(
            &cascade_fields("Solely-owned Net"),
            &sample_connections(),
            account,
            "tok-sole",
            now,
        )
        .await
        .expect("create solely-owned net");

    // --- net_favorites (added later): favorite a net. ---
    let stranger_net = defs
        .create(
            &cascade_fields("Stranger Net"),
            &sample_connections(),
            keeper,
            "tok-stranger",
            now,
        )
        .await
        .expect("create stranger net");
    FavoritesRepo::new(pool.clone())
        .add(account, stranger_net.id, now)
        .await
        .expect("seed favorite");

    // --- session_events survivor: start a session on the STRANGER's net with the
    // account as actor. This writes a session.started row with actor = account
    // that must survive the delete (non-FK actor column). ---
    let survivor_session = match sessions
        .start(&stranger_net, Some(account), now)
        .await
        .expect("start survivor session")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    // --- net_session_roles (added later): grant the account a role on the
    // session — a personal-data row that must cascade. ---
    sqlx::query(
        "INSERT INTO net_session_roles (net_session_id, account_id, role)
         VALUES ($1, $2, 'logger')",
    )
    .bind(survivor_session.id)
    .bind(account)
    .execute(&pool)
    .await
    .expect("seed net_session_role");

    // --- audit_log survivor: a row whose actor is the account (non-FK uuid). ---
    sqlx::query(
        "INSERT INTO audit_log (id, occurred_at, actor_account_id, action)
         VALUES ($1, now(), $2, 'account-self-deleted')",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .execute(&pool)
    .await
    .expect("seed audit_log");

    // --- abuse_reports survivor: a report resolved_by the account (non-FK uuid). ---
    sqlx::query(
        "INSERT INTO abuse_reports (id, created_at, body, resolved_at, resolved_by)
         VALUES ($1, now(), 'historical report', now(), $2)",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .execute(&pool)
    .await
    .expect("seed abuse_report");

    // --- net_definition_roster survivor: callsign-keyed, no account FK. ---
    sqlx::query(
        "INSERT INTO net_definition_roster (definition_id, callsign, last_seen_at)
         VALUES ($1, 'W1AW', now())",
    )
    .bind(stranger_net.id)
    .execute(&pool)
    .await
    .expect("seed roster memory");

    // The eight personal-data cascade tables, shared by the pre- and post-
    // delete assertions below: without a pre-delete "count == 1" guard, a
    // seeding regression that silently wrote zero rows for a table would still
    // satisfy a post-delete "count == 0" assertion vacuously. The whole point
    // of this test is proving eight real rows vanish, not that eight absent
    // rows stay absent.
    let cascade_tables: [(&str, &str); 8] = [
        (
            "auth_methods",
            "SELECT count(*) FROM auth_methods WHERE account_id = $1",
        ),
        (
            "sessions",
            "SELECT count(*) FROM sessions WHERE account_id = $1",
        ),
        (
            "account_consents",
            "SELECT count(*) FROM account_consents WHERE account_id = $1",
        ),
        (
            "email_change_tokens",
            "SELECT count(*) FROM email_change_tokens WHERE account_id = $1",
        ),
        (
            "qrz_credentials",
            "SELECT count(*) FROM qrz_credentials WHERE account_id = $1",
        ),
        (
            "net_favorites",
            "SELECT count(*) FROM net_favorites WHERE account_id = $1",
        ),
        (
            "net_definition_owners",
            "SELECT count(*) FROM net_definition_owners WHERE account_id = $1",
        ),
        (
            "net_session_roles",
            "SELECT count(*) FROM net_session_roles WHERE account_id = $1",
        ),
    ];

    // === Guard: every cascade table genuinely holds at least one seeded row
    // BEFORE deletion runs. (Not asserting an exact count here: most
    // tables hold exactly one row per account, but `net_definition_owners`
    // genuinely holds two — the account owns both the co-owned and the
    // solely-owned net seeded above. `> 0` is the right guard; the exact
    // post-delete row-count-by-net assertions further below already cover
    // the ownership-consequence specifics.) ===
    for (table, sql) in cascade_tables {
        assert!(
            count(&pool, sql, account).await > 0,
            "{table} must genuinely hold at least one seeded row before deletion runs"
        );
    }

    // --- Soft-delete then hard-delete the account (the deletion lifecycle), then run
    // the ownerless-archive sweep the finalizer runs each tick. ---
    accounts
        .soft_delete(account, now)
        .await
        .expect("soft delete");
    assert!(
        accounts.finalize_account(account).await.expect("finalize"),
        "the account row is hard-deleted"
    );
    defs.archive_ownerless(now).await.expect("archive sweep");

    // === All eight personal-data child rows are gone. ===
    for (table, sql) in cascade_tables {
        assert_eq!(
            count(&pool, sql, account).await,
            0,
            "{table} rows must cascade-delete with the account"
        );
    }

    // === Co-owned net survives, keeper still an owner, NOT archived. ===
    let co_owners = count(
        &pool,
        "SELECT count(*) FROM net_definition_owners WHERE net_definition_id = $1",
        co_net.id,
    )
    .await;
    assert_eq!(
        co_owners, 1,
        "the co-owned net keeps exactly the surviving owner"
    );
    let co_keeps_keeper: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM net_definition_owners WHERE net_definition_id = $1 AND account_id = $2",
    )
    .bind(co_net.id)
    .bind(keeper)
    .fetch_one(&pool)
    .await
    .expect("count co-owner keeper");
    assert_eq!(
        co_keeps_keeper, 1,
        "the keeper is still an owner of the co-owned net"
    );
    let co_archived: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT archived_at FROM net_definitions WHERE id = $1")
            .bind(co_net.id)
            .fetch_one(&pool)
            .await
            .expect("read co_net archived_at");
    assert!(co_archived.is_none(), "the co-owned net is NOT archived");

    // === Solely-owned net: zero owners and archived (not hard-deleted). ===
    let sole_owners = count(
        &pool,
        "SELECT count(*) FROM net_definition_owners WHERE net_definition_id = $1",
        sole_net.id,
    )
    .await;
    assert_eq!(
        sole_owners, 0,
        "the solely-owned net has zero owners after finalize"
    );
    let sole_archived: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT archived_at FROM net_definitions WHERE id = $1")
            .bind(sole_net.id)
            .fetch_one(&pool)
            .await
            .expect("read sole_net archived_at");
    assert!(
        sole_archived.is_some(),
        "the solely-owned net is archived, not deleted"
    );

    // === The three non-FK survivors remain untouched. ===
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_log WHERE actor_account_id = $1",
            account
        )
        .await,
        1,
        "the audit_log row survives (actor is a dangling-but-harmless uuid, not an FK)"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM abuse_reports WHERE resolved_by = $1",
            account
        )
        .await,
        1,
        "the abuse_reports row survives (resolved_by is a plain uuid, not an FK)"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM session_events WHERE actor = $1",
            account
        )
        .await,
        1,
        "the session_events row survives (actor is a plain uuid, not an FK)"
    );

    // === Callsign-keyed roster memory (no account FK at all) survives. ===
    let roster: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_definition_roster WHERE callsign = 'W1AW'")
            .fetch_one(&pool)
            .await
            .expect("count roster memory");
    assert_eq!(
        roster, 1,
        "callsign-keyed roster memory is untouched by account deletion"
    );
}
