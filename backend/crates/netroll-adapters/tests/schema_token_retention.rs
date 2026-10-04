// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Retention-pruning tests for the three credential-artifact tables: seed rows
//! on both sides of the cutoff and prove the sweep removes exactly the dead
//! ones. Every assertion reads state back — the identity of the SURVIVING rows,
//! never a returned count. A predicate that deletes three wrong rows and
//! reports `3` must fail here.

use chrono::{DateTime, Utc};
use netroll_adapters::pg::email_changes::EmailChangeRepo;
use netroll_adapters::pg::magic_links::MagicLinkRepo;
use netroll_adapters::pg::sessions::SessionRepo;
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

/// A fixed "now" well clear of the epoch so `now - retention` stays positive
/// without relying on `saturating_sub` to rescue the arithmetic.
const NOW_MILLIS: u64 = 1_800_000_000_000;
/// The shipped default window, expressed in millis.
const RETENTION_MILLIS: u64 = 30 * 24 * 60 * 60 * 1_000;
const DAY_MILLIS: u64 = 24 * 60 * 60 * 1_000;

/// The instant the prune compares against, mirrored in the test so seeded rows
/// can be placed deliberately on either side of it.
fn cutoff_millis() -> u64 {
    NOW_MILLIS - RETENTION_MILLIS
}

fn at(millis: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis as i64).expect("test instant in chrono range")
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

async fn insert_magic_link(pool: &PgPool, email: &str, tag: &str, expires_at_millis: u64) {
    sqlx::query(
        "INSERT INTO magic_link_tokens (id, email, token_hash, expires_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(tag.as_bytes())
    .bind(at(expires_at_millis))
    .execute(pool)
    .await
    .expect("seed magic-link token");
}

async fn insert_email_change(
    pool: &PgPool,
    account_id: Uuid,
    new_email: &str,
    tag: &str,
    expires_at_millis: u64,
) {
    sqlx::query(
        "INSERT INTO email_change_tokens (id, account_id, new_email, token_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(new_email)
    .bind(tag.as_bytes())
    .bind(at(expires_at_millis))
    .execute(pool)
    .await
    .expect("seed email-change token");
}

async fn insert_session(
    pool: &PgPool,
    account_id: Uuid,
    tag: &str,
    last_seen_at_millis: u64,
    absolute_expires_at_millis: u64,
    revoked_at_millis: Option<u64>,
) {
    sqlx::query(
        "INSERT INTO sessions (id, account_id, token_hash, last_seen_at, absolute_expires_at, revoked_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(tag.as_bytes())
    .bind(at(last_seen_at_millis))
    .bind(at(absolute_expires_at_millis))
    .bind(revoked_at_millis.map(at))
    .execute(pool)
    .await
    .expect("seed session");
}

/// Reads back the `token_hash` tags still present, so a test can assert WHICH
/// rows survived rather than merely how many. The query is a literal per table
/// because sqlx 0.9 refuses non-`'static` SQL strings.
async fn surviving_tags(pool: &PgPool, sql: &'static str) -> Vec<String> {
    let rows: Vec<Vec<u8>> = sqlx::query_scalar(sql)
        .fetch_all(pool)
        .await
        .expect("read surviving token hashes");
    rows.into_iter()
        .map(|bytes| String::from_utf8(bytes).expect("test tags are utf-8"))
        .collect()
}

const SURVIVING_MAGIC_LINKS: &str = "SELECT token_hash FROM magic_link_tokens ORDER BY token_hash";
const SURVIVING_SESSIONS: &str = "SELECT token_hash FROM sessions ORDER BY token_hash";
const SURVIVING_EMAIL_CHANGES: &str =
    "SELECT token_hash FROM email_change_tokens ORDER BY token_hash";

#[tokio::test]
async fn prune_removes_rows_past_the_window_and_keeps_rows_inside_it() {
    // The retention window's own behaviour, on all three tables at once: dead
    // BEFORE the cutoff goes, dead AFTER the cutoff (still inside the window)
    // stays. This is what "the retention window is explicit" means in practice.
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "op@example.com").await;
    let cutoff = cutoff_millis();

    insert_magic_link(&pool, "op@example.com", "ml-past", cutoff - DAY_MILLIS).await;
    insert_magic_link(&pool, "op@example.com", "ml-inside", cutoff + DAY_MILLIS).await;
    insert_email_change(
        &pool,
        account_id,
        "next@example.com",
        "ec-past",
        cutoff - DAY_MILLIS,
    )
    .await;
    insert_email_change(
        &pool,
        account_id,
        "later@example.com",
        "ec-inside",
        cutoff + DAY_MILLIS,
    )
    .await;
    insert_session(
        &pool,
        account_id,
        "se-past",
        cutoff - DAY_MILLIS,
        cutoff - DAY_MILLIS,
        None,
    )
    .await;
    insert_session(
        &pool,
        account_id,
        "se-inside",
        cutoff + DAY_MILLIS,
        cutoff + DAY_MILLIS,
        None,
    )
    .await;

    let magic_links = MagicLinkRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool.clone());

    let pruned_magic_links = magic_links
        .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
        .await
        .expect("prune magic-link tokens");
    let pruned_sessions = sessions
        .prune_dead(NOW_MILLIS, RETENTION_MILLIS)
        .await
        .expect("prune sessions");
    let pruned_email_changes = email_changes
        .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
        .await
        .expect("prune email-change tokens");

    assert_eq!(pruned_magic_links, 1);
    assert_eq!(pruned_sessions, 1);
    assert_eq!(pruned_email_changes, 1);

    assert_eq!(
        surviving_tags(&pool, SURVIVING_MAGIC_LINKS).await,
        vec!["ml-inside".to_owned()],
        "only the past-window magic-link row may be removed"
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_SESSIONS).await,
        vec!["se-inside".to_owned()],
        "only the past-window session may be removed"
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_EMAIL_CHANGES).await,
        vec!["ec-inside".to_owned()],
        "only the past-window email-change row may be removed"
    );
}

#[tokio::test]
async fn the_cutoff_instant_itself_survives_on_every_table_and_both_session_arms() {
    // All three predicates use a STRICT `<`, matching the consume paths'
    // convention of refusing at exactly `expires_at`. Until this test existed
    // that invariant lived only in a comment: relaxing `<` to `<=` in all three
    // repos passed the whole suite, because every other test seeds `cutoff - 1`
    // and never the boundary instant itself. The governing lesson is
    // that an invariant claimed by a comment must be asserted by a test.
    //
    // Each table gets the three instants that straddle the boundary — one
    // millisecond before, exactly at, one millisecond after — and the sessions
    // table gets them TWICE, once per arm of its `OR`, so neither arm can drift
    // to `<=` unnoticed.
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "boundary@example.com").await;
    let cutoff = cutoff_millis();
    let far_future = NOW_MILLIS + 30 * DAY_MILLIS;

    for (tag, expires_at) in [
        ("ml-before", cutoff - 1),
        ("ml-at", cutoff),
        ("ml-after", cutoff + 1),
    ] {
        insert_magic_link(&pool, "boundary@example.com", tag, expires_at).await;
    }
    for (tag, expires_at) in [
        ("ec-before", cutoff - 1),
        ("ec-at", cutoff),
        ("ec-after", cutoff + 1),
    ] {
        insert_email_change(
            &pool,
            account_id,
            &format!("{tag}@example.com"),
            tag,
            expires_at,
        )
        .await;
    }
    // Arm one: `absolute_expires_at < cutoff`, never revoked.
    for (tag, absolute_expires_at) in [
        ("se-abs-before", cutoff - 1),
        ("se-abs-at", cutoff),
        ("se-abs-after", cutoff + 1),
    ] {
        insert_session(&pool, account_id, tag, cutoff, absolute_expires_at, None).await;
    }
    // Arm two: `revoked_at < cutoff`, with the absolute cap held far in the
    // future so ONLY the revoked arm can select these rows.
    for (tag, revoked_at) in [
        ("se-rev-before", cutoff - 1),
        ("se-rev-at", cutoff),
        ("se-rev-after", cutoff + 1),
    ] {
        insert_session(&pool, account_id, tag, cutoff, far_future, Some(revoked_at)).await;
    }

    let magic_links = MagicLinkRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool.clone());

    assert_eq!(
        magic_links
            .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune magic-link tokens"),
        1,
        "only the row strictly before the cutoff may go"
    );
    assert_eq!(
        sessions
            .prune_dead(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune sessions"),
        2,
        "one row per arm, each strictly before the cutoff"
    );
    assert_eq!(
        email_changes
            .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune email-change tokens"),
        1,
        "only the row strictly before the cutoff may go"
    );

    assert_eq!(
        surviving_tags(&pool, SURVIVING_MAGIC_LINKS).await,
        vec!["ml-after".to_owned(), "ml-at".to_owned()],
        "a magic-link token expiring exactly AT the cutoff must survive"
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_EMAIL_CHANGES).await,
        vec!["ec-after".to_owned(), "ec-at".to_owned()],
        "an email-change token expiring exactly AT the cutoff must survive"
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_SESSIONS).await,
        vec![
            "se-abs-after".to_owned(),
            "se-abs-at".to_owned(),
            "se-rev-after".to_owned(),
            "se-rev-at".to_owned(),
        ],
        "a session capped or revoked exactly AT the cutoff must survive, on both arms"
    );
}

#[tokio::test]
async fn prune_leaves_still_valid_tokens_and_live_sessions_untouched() {
    // The "pruning must never sign a live user out" guard. Everything here is
    // still usable at NOW_MILLIS; a sweep must not touch any of it.
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "live@example.com").await;

    insert_magic_link(&pool, "live@example.com", "ml-valid", NOW_MILLIS + 900_000).await;
    insert_email_change(
        &pool,
        account_id,
        "next@example.com",
        "ec-valid",
        NOW_MILLIS + 900_000,
    )
    .await;
    insert_session(
        &pool,
        account_id,
        "se-live",
        NOW_MILLIS,
        NOW_MILLIS + 30 * DAY_MILLIS,
        None,
    )
    .await;

    let magic_links = MagicLinkRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool.clone());

    assert_eq!(
        magic_links
            .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune magic-link tokens"),
        0
    );
    assert_eq!(
        sessions
            .prune_dead(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune sessions"),
        0
    );
    assert_eq!(
        email_changes
            .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune email-change tokens"),
        0
    );

    assert_eq!(
        surviving_tags(&pool, SURVIVING_MAGIC_LINKS).await,
        vec!["ml-valid".to_owned()]
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_SESSIONS).await,
        vec!["se-live".to_owned()],
        "a session inside its absolute cap and never revoked must survive"
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_EMAIL_CHANGES).await,
        vec!["ec-valid".to_owned()]
    );
}

#[tokio::test]
async fn a_revoked_session_is_pruned_even_though_its_absolute_cap_is_still_far_future() {
    // SESSION_ABSOLUTE_MILLIS is 30 days, so a session revoked one minute after
    // sign-in keeps an absolute cap up to 30 days out. Without the `revoked_at`
    // arm every sign-out and bulk revoke leaves a row for its full original
    // lifetime — deleting this arm from the predicate must fail this test.
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "signed-out@example.com").await;

    insert_session(
        &pool,
        account_id,
        "se-revoked",
        cutoff_millis() - DAY_MILLIS,
        NOW_MILLIS + 30 * DAY_MILLIS,
        Some(cutoff_millis() - 1),
    )
    .await;

    let sessions = SessionRepo::new(pool.clone());
    assert_eq!(
        sessions
            .prune_dead(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune sessions"),
        1
    );
    assert!(
        surviving_tags(&pool, SURVIVING_SESSIONS).await.is_empty(),
        "a long-revoked session must not linger for its original absolute lifetime"
    );
}

#[tokio::test]
async fn an_idle_session_inside_its_absolute_cap_is_not_pruned() {
    // `last_seen_at` is a DOMAIN verdict (idle expiry, judged per request
    // against a tunable window), not the pruner's to own. Adding it to the
    // predicate would couple physical row lifetime to a policy knob — this test
    // exists so that "optimization" fails loudly.
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "idle@example.com").await;

    insert_session(
        &pool,
        account_id,
        "se-idle",
        NOW_MILLIS - 100 * DAY_MILLIS,
        NOW_MILLIS + DAY_MILLIS,
        None,
    )
    .await;

    let sessions = SessionRepo::new(pool.clone());
    assert_eq!(
        sessions
            .prune_dead(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune sessions"),
        0
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_SESSIONS).await,
        vec!["se-idle".to_owned()],
        "idle-but-uncapped sessions are the domain's business, not the pruner's"
    );
}

#[tokio::test]
async fn magic_link_rows_orphaned_by_a_finalized_account_are_pruned() {
    // Both halves in order. ON DELETE CASCADE recreated four
    // FKs; magic_link_tokens has none to recreate (it is email-keyed), so its
    // rows SURVIVE a finalize. The prune reaches them anyway because its
    // predicate is purely time-based and account-independent by construction.
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "gone@example.com").await;
    insert_magic_link(
        &pool,
        "gone@example.com",
        "ml-orphan",
        cutoff_millis() - DAY_MILLIS,
    )
    .await;

    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("finalize (hard-delete) the account");

    assert_eq!(
        surviving_tags(&pool, SURVIVING_MAGIC_LINKS).await,
        vec!["ml-orphan".to_owned()],
        "the cascade does not reach an email-keyed table (the 1.11 behaviour)"
    );

    let magic_links = MagicLinkRepo::new(pool.clone());
    assert_eq!(
        magic_links
            .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune magic-link tokens"),
        1
    );
    assert!(
        surviving_tags(&pool, SURVIVING_MAGIC_LINKS)
            .await
            .is_empty(),
        "the time-based sweep reaches what the cascade structurally cannot"
    );
}

#[tokio::test]
async fn a_live_never_deleted_accounts_stale_rows_are_pruned() {
    // The leak no existing mechanism addresses. ON DELETE CASCADE fires
    // only on finalization; for an account that is never deleted it never fires
    // at all, so every consumed link, revoked session and email-change token
    // accretes permanently.
    let (_container, pool) = migrated_pool().await;
    let account_id = insert_account(&pool, "regular@example.com").await;
    let cutoff = cutoff_millis();

    insert_magic_link(
        &pool,
        "regular@example.com",
        "ml-stale",
        cutoff - DAY_MILLIS,
    )
    .await;
    insert_magic_link(
        &pool,
        "regular@example.com",
        "ml-current",
        NOW_MILLIS + 900_000,
    )
    .await;
    insert_email_change(
        &pool,
        account_id,
        "next@example.com",
        "ec-stale",
        cutoff - DAY_MILLIS,
    )
    .await;
    insert_email_change(
        &pool,
        account_id,
        "later@example.com",
        "ec-current",
        NOW_MILLIS + 900_000,
    )
    .await;
    insert_session(
        &pool,
        account_id,
        "se-revoked",
        cutoff - DAY_MILLIS,
        NOW_MILLIS + 30 * DAY_MILLIS,
        Some(cutoff - DAY_MILLIS),
    )
    .await;
    insert_session(
        &pool,
        account_id,
        "se-current",
        NOW_MILLIS,
        NOW_MILLIS + 30 * DAY_MILLIS,
        None,
    )
    .await;

    let magic_links = MagicLinkRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool.clone());

    assert_eq!(
        magic_links
            .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune magic-link tokens"),
        1
    );
    assert_eq!(
        sessions
            .prune_dead(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune sessions"),
        1
    );
    assert_eq!(
        email_changes
            .prune_expired(NOW_MILLIS, RETENTION_MILLIS)
            .await
            .expect("prune email-change tokens"),
        1
    );

    assert_eq!(
        surviving_tags(&pool, SURVIVING_MAGIC_LINKS).await,
        vec!["ml-current".to_owned()]
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_SESSIONS).await,
        vec!["se-current".to_owned()]
    );
    assert_eq!(
        surviving_tags(&pool, SURVIVING_EMAIL_CHANGES).await,
        vec!["ec-current".to_owned()]
    );

    let account_still_there: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .expect("count accounts");
    assert_eq!(
        account_still_there, 1,
        "pruning credential artifacts must never touch the account itself"
    );
}
