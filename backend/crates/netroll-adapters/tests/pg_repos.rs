// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Integration tests for the Postgres repos against a real
//! containerized Postgres (no mocked SQL). Single-use consumption is
//! proven at the database level — one atomic UPDATE, not read-then-write.

use netroll_adapters::pg::accounts::{AccountRepo, SetCallsignOutcome, SoftDeleteOutcome};
use netroll_adapters::pg::consents::ConsentRepo;
use netroll_adapters::pg::delivery_configs::DeliveryConfigRepo;
use netroll_adapters::pg::delivery_jobs::{DeliveryJobRepo, TerminalState};
use netroll_adapters::pg::discovery::DiscoveryRepo;
use netroll_adapters::pg::email_changes::{ConfirmEmailChangeOutcome, EmailChangeRepo};
use netroll_adapters::pg::favorites::FavoritesRepo;
use netroll_adapters::pg::magic_links::MagicLinkRepo;
use netroll_adapters::pg::net_definitions::{
    AddOwnerOutcome, NetDefinitionRepo, RemoveOwnerOutcome,
};
use netroll_adapters::pg::net_session_roles::{NetSessionRoleRepo, RevokeOutcome, RoleGrant};
use netroll_adapters::pg::net_sessions::{
    AddCheckInOutcome, ChangeFrequencyOutcome, ClaimControlOutcome, CloseOutcome,
    EditCheckInOutcome, HandoffOutcome, ModerateOutcome, NetSessionRepo, NoteOutcome,
    OrderModeOutcome, ReorderOutcome, ResumeOutcome, StallOutcome, StartOutcome,
    WorkedStationOutcome,
};
use netroll_adapters::pg::roster_memory::RosterMemoryRepo;
use netroll_adapters::pg::schedules::{MATERIALIZATION_HORIZON_MILLIS, ScheduleRepo};
use netroll_adapters::pg::session_events::SessionEventLog;
use netroll_adapters::pg::sessions::SessionRepo;
use netroll_domain::admin::{MAX_PAGE_LIMIT, clamp_limit};
use netroll_domain::auth::{
    MagicLinkVerdict, SESSION_IDLE_MILLIS, SessionVerdict, hash_token, magic_link_verdict,
    session_verdict,
};
use netroll_domain::authz::Role;
use netroll_domain::callsign::parse_callsign;
use netroll_domain::check_in::{
    CheckInSource, Precedence, StayingStatus, parse_location, parse_name, parse_signal_report,
    parse_traffic_count,
};
use netroll_domain::consent::{ConsentRecord, ConsentVerdict, consent_verdict};
use netroll_domain::event::{SessionEvent, SessionEventBody};
use netroll_domain::fold::{ControlState, SessionLifecycle};
use netroll_domain::model::account::ProfileFields;
use netroll_domain::net::connection::{NetConnection, NetConnectionKind, NetConnectionSet};
use netroll_domain::net::delivery::DeliveryConfigFields;
use netroll_domain::net::discovery::{
    ConnectionKindFilter, DiscoveryFilters, DiscoveryQuery, DiscoverySort,
};
use netroll_domain::net::enums::{Band, Mode, NetCategory, NetType, Visibility};
use netroll_domain::net::schedule::{Frequency, RecurringSchedule, Schedule};
use netroll_domain::net::validation::{
    NetDefinitionFields, RawNetDefinition, parse_net_definition_fields,
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const HOUR_MILLIS: u64 = 60 * 60 * 1000;

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
        // The default (10) already lets same-row concurrent appends race at
        // the Postgres row-lock level (any 2+ simultaneously-held connections
        // suffice to prove the lock serializes them). Raised so the 50-way
        // concurrency test below can have every task in flight against
        // Postgres at once, rather than the default 10 at a time — a
        // stronger, unambiguous exercise of the same guarantee.
        .max_connections(50)
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

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64
}

#[tokio::test]
async fn magic_link_consumption_is_single_use() {
    let (_container, pool) = migrated_pool().await;
    let repo = MagicLinkRepo::new(pool);
    let hash = hash_token(b"raw-token-1");
    let now = now_millis();

    repo.issue("op@example.com", hash, now + HOUR_MILLIS)
        .await
        .expect("issue link");

    let first = repo.consume(hash, now).await.expect("first consume query");
    let second = repo.consume(hash, now).await.expect("second consume query");

    assert_eq!(first.as_deref(), Some("op@example.com"));
    assert_eq!(second, None, "second use must be rejected by the database");
}

#[tokio::test]
async fn expired_magic_link_cannot_be_consumed() {
    let (_container, pool) = migrated_pool().await;
    let repo = MagicLinkRepo::new(pool);
    let hash = hash_token(b"raw-token-2");
    let now = now_millis();

    repo.issue("op@example.com", hash, now - HOUR_MILLIS)
        .await
        .expect("issue link");

    let consumed = repo.consume(hash, now).await.expect("consume query");
    assert_eq!(
        consumed, None,
        "the expiry guard lives in the UPDATE itself"
    );
}

#[tokio::test]
async fn consume_judges_expiry_by_the_injected_clock_not_the_database_clock() {
    let (_container, pool) = migrated_pool().await;
    let repo = MagicLinkRepo::new(pool);
    let hash = hash_token(b"raw-token-clock");
    let expires_at = now_millis() + HOUR_MILLIS;

    repo.issue("op@example.com", hash, expires_at)
        .await
        .expect("issue link");

    // The caller's clock is authoritative: at the boundary instant the
    // token is expired (matches `magic_link_verdict`), regardless of what
    // Postgres' own wall clock says.
    let at_boundary = repo
        .consume(hash, expires_at)
        .await
        .expect("boundary consume query");
    assert_eq!(at_boundary, None, "now == expires_at must refuse");

    let just_before = repo
        .consume(hash, expires_at - 1)
        .await
        .expect("consume query");
    assert_eq!(just_before.as_deref(), Some("op@example.com"));
}

#[tokio::test]
async fn stored_link_maps_to_a_domain_token_the_verdict_accepts() {
    let (_container, pool) = migrated_pool().await;
    let repo = MagicLinkRepo::new(pool);
    let hash = hash_token(b"raw-token-3");
    let now = now_millis();

    repo.issue("op@example.com", hash, now + HOUR_MILLIS)
        .await
        .expect("issue link");

    let found = repo.find(hash).await.expect("find query");
    assert_eq!(
        magic_link_verdict(found.as_ref(), now),
        MagicLinkVerdict::Valid
    );
    let missing = repo.find(hash_token(b"nope")).await.expect("find query");
    assert_eq!(
        magic_link_verdict(missing.as_ref(), now),
        MagicLinkVerdict::Invalid
    );
}

#[tokio::test]
async fn create_verified_and_attach_is_idempotent_and_marks_verification() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool.clone());
    let now = now_millis();

    let first = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("first find-or-create");
    let second = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("second find-or-create converges on the same account");

    assert_eq!(first.id, second.id, "same email must map to one account");
    assert!(
        first.email_verified_at_millis.is_some(),
        "consuming a link marks the email verified"
    );

    let methods: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auth_methods WHERE account_id = $1")
            .bind(first.id)
            .fetch_one(&pool)
            .await
            .expect("count auth methods");
    assert_eq!(methods, 1, "exactly one magic-link method attaches");
}

#[tokio::test]
async fn verify_and_attach_marks_an_existing_account_verified() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool.clone());
    let now = now_millis();

    // An account that exists but was never verified (models the email-change
    // reuse path; today accounts are always created verified).
    let id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, $2)")
        .bind(id)
        .bind("op@example.com")
        .execute(&pool)
        .await
        .expect("seed account");

    repo.verify_and_attach(id, now).await.expect("verify");

    let account = repo
        .find_by_email("op@example.com")
        .await
        .expect("query")
        .expect("account exists");
    assert!(account.email_verified_at_millis.is_some());
    let methods: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auth_methods WHERE account_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("count methods");
    assert_eq!(methods, 1);
}

#[tokio::test]
async fn find_by_email_returns_only_existing_accounts() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    assert_eq!(
        repo.find_by_email("op@example.com").await.expect("query"),
        None,
        "requesting a link must not create an account"
    );

    let created = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create");
    let found = repo
        .find_by_email("op@example.com")
        .await
        .expect("query")
        .expect("account exists after consume");
    assert_eq!(found.id, created.id);
}

#[tokio::test]
async fn recording_consent_is_idempotent_under_double_click() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let consents = ConsentRepo::new(pool.clone());
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    let record = ConsentRecord {
        terms_version: "2026-07-15".into(),
        consented_at_millis: now,
    };

    let first_inserted = consents
        .record(account.id, &record)
        .await
        .expect("first record succeeds");
    let second_inserted = consents
        .record(account.id, &record)
        .await
        .expect("second record of the same version succeeds without error");

    assert!(first_inserted, "the first record is a real insert");
    assert!(
        !second_inserted,
        "the duplicate record is a no-op the caller can distinguish"
    );

    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_consents WHERE account_id = $1")
            .bind(account.id)
            .fetch_one(&pool)
            .await
            .expect("count consents");
    assert_eq!(rows, 1, "double-click must not duplicate the consent row");
}

#[tokio::test]
async fn consented_versions_feed_the_domain_verdict() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let consents = ConsentRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");

    // No rows reads as unconsented.
    let none = consents
        .consented_versions(account.id)
        .await
        .expect("lookup");
    assert_eq!(
        consent_verdict(&none, "2026-07-15"),
        ConsentVerdict::ConsentRequired
    );

    // The domain-decided timestamp round-trips through storage untouched.
    consents
        .record(
            account.id,
            &ConsentRecord {
                terms_version: "2026-07-15".into(),
                consented_at_millis: 1_752_540_000_000,
            },
        )
        .await
        .expect("record consent");

    let versions = consents
        .consented_versions(account.id)
        .await
        .expect("lookup");
    assert_eq!(
        consent_verdict(&versions, "2026-07-15"),
        ConsentVerdict::Consented
    );
}

#[tokio::test]
async fn consent_to_an_older_version_does_not_satisfy_a_newer_requirement() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let consents = ConsentRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");

    // The account consented once, but to a version the server no longer
    // requires — the security property this whole story exists for.
    consents
        .record(
            account.id,
            &ConsentRecord {
                terms_version: "2025-01-01".into(),
                consented_at_millis: now,
            },
        )
        .await
        .expect("record consent to the older version");

    let versions = consents
        .consented_versions(account.id)
        .await
        .expect("lookup");
    assert_eq!(
        consent_verdict(&versions, "2026-07-15"),
        ConsentVerdict::ConsentRequired,
        "consent to an older version must not carry forward to the current one"
    );
}

#[tokio::test]
async fn sessions_round_trip_revoke_and_reject() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    let hash = hash_token(b"session-token");

    sessions
        .insert(account.id, hash, now, now + HOUR_MILLIS)
        .await
        .expect("insert session");

    let live = sessions
        .find(hash)
        .await
        .expect("find query")
        .expect("session exists");
    assert_eq!(live.account_id, account.id);
    assert_eq!(
        session_verdict(&live.state, SESSION_IDLE_MILLIS, now + 1),
        SessionVerdict::Valid
    );

    let newly_revoked = sessions.revoke(hash).await.expect("revoke");
    assert!(newly_revoked, "the first revoke actually flipped the row");
    let revoked = sessions
        .find(hash)
        .await
        .expect("find query")
        .expect("revoked session row still exists server-side");
    assert_eq!(
        session_verdict(&revoked.state, SESSION_IDLE_MILLIS, now + 1),
        SessionVerdict::Rejected,
        "a revoked session must never authenticate"
    );

    // A repeat revoke (e.g. a concurrent racing sign-out) is a no-op that
    // reports `false` — the caller uses this to avoid double-auditing the
    // same sign-out.
    let repeat_revoked = sessions.revoke(hash).await.expect("revoke again");
    assert!(
        !repeat_revoked,
        "a repeat revoke reports no new state change"
    );

    let unknown = sessions.find(hash_token(b"other")).await.expect("query");
    assert!(unknown.is_none());
}

#[tokio::test]
async fn set_callsign_persists_and_is_readable_by_id() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let account = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");

    let outcome = repo
        .set_callsign(account.id, "W1AW", now)
        .await
        .expect("set callsign query");
    assert_eq!(outcome, SetCallsignOutcome::Reserved);

    let found = repo
        .find_by_id(account.id)
        .await
        .expect("query")
        .expect("account exists");
    assert_eq!(found.callsign.as_deref(), Some("W1AW"));
}

#[tokio::test]
async fn reserving_a_callsign_held_by_another_account_is_taken_not_a_raw_error() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let holder = repo
        .create_verified_and_attach("holder@example.com", now)
        .await
        .expect("create holder");
    repo.set_callsign(holder.id, "W1AW", now)
        .await
        .expect("holder reserves W1AW");

    let challenger = repo
        .create_verified_and_attach("challenger@example.com", now)
        .await
        .expect("create challenger");
    let outcome = repo
        .set_callsign(challenger.id, "W1AW", now)
        .await
        .expect("set callsign query must not surface a raw sqlx::Error");
    assert_eq!(outcome, SetCallsignOutcome::Taken);
}

#[tokio::test]
async fn changing_a_callsign_frees_it_for_another_account() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let a = repo
        .create_verified_and_attach("a@example.com", now)
        .await
        .expect("create A");
    let b = repo
        .create_verified_and_attach("b@example.com", now)
        .await
        .expect("create B");

    repo.set_callsign(a.id, "W1AW", now)
        .await
        .expect("A reserves W1AW");
    repo.set_callsign(a.id, "K1ABC", now)
        .await
        .expect("A changes to K1ABC");
    let outcome = repo
        .set_callsign(b.id, "W1AW", now)
        .await
        .expect("B reserves the freed W1AW");
    assert_eq!(outcome, SetCallsignOutcome::Reserved);

    let a_now = repo
        .find_by_id(a.id)
        .await
        .expect("query")
        .expect("A exists");
    let b_now = repo
        .find_by_id(b.id)
        .await
        .expect("query")
        .expect("B exists");
    assert_eq!(a_now.callsign.as_deref(), Some("K1ABC"));
    assert_eq!(b_now.callsign.as_deref(), Some("W1AW"));
}

#[tokio::test]
async fn setting_the_callsign_an_account_already_holds_is_idempotent() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let account = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    repo.set_callsign(account.id, "W1AW", now)
        .await
        .expect("first reservation");

    let outcome = repo
        .set_callsign(account.id, "W1AW", now)
        .await
        .expect("re-setting the same held value must not self-conflict on the unique index");
    assert_eq!(outcome, SetCallsignOutcome::Reserved);
}

#[tokio::test]
async fn setting_a_callsign_for_a_nonexistent_account_is_an_error_not_a_silent_success() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    // No account created — the UPDATE matches zero rows. This must not be
    // reported as `Reserved`: a zero-row write is not a successful claim.
    let ghost_id = uuid::Uuid::now_v7();
    let result = repo.set_callsign(ghost_id, "W1AW", now).await;

    assert!(
        result.is_err(),
        "a zero-row UPDATE must surface as an error, not SetCallsignOutcome::Reserved"
    );
}

#[tokio::test]
async fn concurrent_claims_of_the_same_callsign_by_different_accounts_yield_exactly_one_winner() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let a = repo
        .create_verified_and_attach("racer-a@example.com", now)
        .await
        .expect("create A");
    let b = repo
        .create_verified_and_attach("racer-b@example.com", now)
        .await
        .expect("create B");

    let repo_a = repo.clone();
    let repo_b = repo.clone();
    let (result_a, result_b) = tokio::join!(
        async move { repo_a.set_callsign(a.id, "W1AW", now).await },
        async move { repo_b.set_callsign(b.id, "W1AW", now).await },
    );

    let outcomes = [
        result_a.expect("racing set_callsign must not surface a raw error"),
        result_b.expect("racing set_callsign must not surface a raw error"),
    ];
    let reserved_count = outcomes
        .iter()
        .filter(|o| **o == SetCallsignOutcome::Reserved)
        .count();
    let taken_count = outcomes
        .iter()
        .filter(|o| **o == SetCallsignOutcome::Taken)
        .count();
    assert_eq!(reserved_count, 1, "exactly one racer must win the claim");
    assert_eq!(taken_count, 1, "the loser must observe Taken, not an error");
}

#[tokio::test]
async fn touch_advances_the_idle_cursor() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    let hash = hash_token(b"session-token");
    sessions
        .insert(account.id, hash, now, now + HOUR_MILLIS)
        .await
        .expect("insert session");

    sessions.touch(hash, now + 5_000).await.expect("touch");

    let row = sessions
        .find(hash)
        .await
        .expect("query")
        .expect("session exists");
    assert_eq!(row.state.last_seen_at_millis, now + 5_000);
}

/// A fully-populated profile write shape for the `update_profile` tests.
fn full_profile() -> ProfileFields {
    ProfileFields {
        display_name: Some("Maria".to_owned()),
        location: Some("Hartford, CT".to_owned()),
        grid: Some("FN31pr".to_owned()),
        avatar_url: Some("https://example.com/me.png".to_owned()),
    }
}

#[tokio::test]
async fn update_profile_persists_and_every_read_path_round_trips_it() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let created = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    assert_eq!(
        (
            created.display_name,
            created.location,
            created.grid,
            created.avatar_url
        ),
        (None, None, None, None),
        "a fresh account carries no profile values"
    );

    repo.update_profile(created.id, &full_profile(), now)
        .await
        .expect("update profile");

    for account in [
        repo.find_by_id(created.id)
            .await
            .expect("query")
            .expect("account exists"),
        repo.find_by_email("op@example.com")
            .await
            .expect("query")
            .expect("account exists"),
        repo.create_verified_and_attach("op@example.com", now)
            .await
            .expect("idempotent re-create returns the same account"),
    ] {
        assert_eq!(account.display_name.as_deref(), Some("Maria"));
        assert_eq!(account.location.as_deref(), Some("Hartford, CT"));
        assert_eq!(account.grid.as_deref(), Some("FN31pr"));
        assert_eq!(
            account.avatar_url.as_deref(),
            Some("https://example.com/me.png")
        );
    }
}

#[tokio::test]
async fn update_profile_with_none_clears_previously_set_fields() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let account = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    repo.update_profile(account.id, &full_profile(), now)
        .await
        .expect("populate profile");

    // PUT-replace semantics: the adapter writes exactly what it is given —
    // a None field nulls the column, no COALESCE.
    repo.update_profile(
        account.id,
        &ProfileFields {
            display_name: None,
            location: Some("Boston, MA".to_owned()),
            grid: None,
            avatar_url: None,
        },
        now,
    )
    .await
    .expect("partial replace");

    let read = repo
        .find_by_id(account.id)
        .await
        .expect("query")
        .expect("account exists");
    assert_eq!(read.display_name, None, "None clears a set field");
    assert_eq!(read.location.as_deref(), Some("Boston, MA"));
    assert_eq!(read.grid, None);
    assert_eq!(read.avatar_url, None);
}

#[tokio::test]
async fn update_profile_on_a_nonexistent_account_is_row_not_found() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let result = repo
        .update_profile(uuid::Uuid::now_v7(), &full_profile(), now)
        .await;

    assert!(
        matches!(result, Err(sqlx::Error::RowNotFound)),
        "a zero-row UPDATE must not be reported as success (1.8 review lesson), got {result:?}"
    );
}

async fn account_updated_at_millis(pool: &PgPool, account_id: uuid::Uuid) -> u64 {
    let updated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(pool)
            .await
            .expect("read updated_at");
    updated_at.timestamp_millis().max(0) as u64
}

#[tokio::test]
async fn email_change_issue_and_find_round_trip_the_domain_view() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("old@example.com", now)
        .await
        .expect("create account");
    let hash = hash_token(b"change-token-1");
    let expires_at = now + HOUR_MILLIS;

    email_changes
        .issue(account.id, "new@example.com", hash, expires_at)
        .await
        .expect("issue token");

    let found = email_changes
        .find(hash)
        .await
        .expect("find query")
        .expect("token exists");
    assert_eq!(found.account_id, account.id);
    assert_eq!(found.new_email, "new@example.com");
    assert_eq!(found.expires_at_millis, expires_at);
    assert_eq!(found.consumed_at_millis, None);
}

#[tokio::test]
async fn confirming_an_email_change_swaps_the_email_reverifies_and_consumes_the_token() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool.clone());
    let created_at = now_millis();

    let account = accounts
        .create_verified_and_attach("old@example.com", created_at)
        .await
        .expect("create account");
    let hash = hash_token(b"change-token-happy");
    email_changes
        .issue(
            account.id,
            "new@example.com",
            hash,
            created_at + HOUR_MILLIS,
        )
        .await
        .expect("issue token");

    let confirm_at = created_at + 5_000;
    let outcome = email_changes
        .confirm(hash, confirm_at)
        .await
        .expect("confirm query");
    assert_eq!(
        outcome,
        ConfirmEmailChangeOutcome::Changed {
            account_id: account.id,
            old_email: "old@example.com".to_owned(),
            new_email: "new@example.com".to_owned(),
        }
    );

    let updated = accounts
        .find_by_id(account.id)
        .await
        .expect("query")
        .expect("account exists");
    assert_eq!(updated.email, "new@example.com", "email is swapped");
    assert_eq!(
        updated.email_verified_at_millis,
        Some(confirm_at),
        "the consumed link IS the fresh verification instant"
    );
    assert_eq!(
        account_updated_at_millis(&pool, account.id).await,
        confirm_at,
        "updated_at advances to the confirm instant"
    );

    let token = email_changes
        .find(hash)
        .await
        .expect("find query")
        .expect("token row survives");
    assert_eq!(
        token.consumed_at_millis,
        Some(confirm_at),
        "the token is marked consumed at the confirm instant"
    );
}

#[tokio::test]
async fn confirming_an_email_change_revokes_only_the_accounts_own_sessions() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("old@example.com", now)
        .await
        .expect("create account");
    let bystander = accounts
        .create_verified_and_attach("bystander@example.com", now)
        .await
        .expect("create bystander");

    let session_a = hash_token(b"session-a");
    let session_b = hash_token(b"session-b");
    let bystander_session = hash_token(b"bystander-session");
    for (account_id, hash) in [
        (account.id, session_a),
        (account.id, session_b),
        (bystander.id, bystander_session),
    ] {
        sessions
            .insert(account_id, hash, now, now + HOUR_MILLIS)
            .await
            .expect("insert session");
    }

    let hash = hash_token(b"change-token-sessions");
    email_changes
        .issue(account.id, "new@example.com", hash, now + HOUR_MILLIS)
        .await
        .expect("issue token");
    email_changes
        .confirm(hash, now + 1)
        .await
        .expect("confirm query");

    for revoked in [session_a, session_b] {
        let row = sessions
            .find(revoked)
            .await
            .expect("query")
            .expect("session row exists");
        assert_eq!(
            session_verdict(&row.state, SESSION_IDLE_MILLIS, now + 2),
            SessionVerdict::Rejected,
            "the account's own sessions are all revoked"
        );
    }
    let bystander_row = sessions
        .find(bystander_session)
        .await
        .expect("query")
        .expect("bystander session exists");
    assert_eq!(
        session_verdict(&bystander_row.state, SESSION_IDLE_MILLIS, now + 2),
        SessionVerdict::Valid,
        "a bystander's session must be untouched"
    );
}

#[tokio::test]
async fn confirming_an_email_change_is_single_use() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("old@example.com", now)
        .await
        .expect("create account");
    let hash = hash_token(b"change-token-race");
    email_changes
        .issue(account.id, "new@example.com", hash, now + HOUR_MILLIS)
        .await
        .expect("issue token");

    let first = email_changes.confirm(hash, now + 1).await.expect("first");
    let second = email_changes.confirm(hash, now + 2).await.expect("second");

    assert!(
        matches!(first, ConfirmEmailChangeOutcome::Changed { .. }),
        "first confirm changes the email, got {first:?}"
    );
    assert_eq!(
        second,
        ConfirmEmailChangeOutcome::NotConsumable,
        "the second confirm of a consumed token is refused"
    );
}

#[tokio::test]
async fn confirming_a_second_pending_token_reports_the_email_the_first_swap_just_set_as_old() {
    // Multiple pending tokens per account coexist by design (no per-account
    // uniqueness — Dev Notes). Confirming the first retargets the account;
    // confirming the second must report the address the FIRST confirm just
    // set as "old" — never the account's original email from before either
    // token was issued. A pre-transaction read of "old email" would go
    // stale exactly here; `confirm` reads it inside its own transaction.
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("original@example.com", now)
        .await
        .expect("create account");

    let hash_first = hash_token(b"change-token-sibling-first");
    let hash_second = hash_token(b"change-token-sibling-second");
    email_changes
        .issue(
            account.id,
            "middle@example.com",
            hash_first,
            now + HOUR_MILLIS,
        )
        .await
        .expect("issue first token");
    email_changes
        .issue(
            account.id,
            "final@example.com",
            hash_second,
            now + HOUR_MILLIS,
        )
        .await
        .expect("issue second token");

    let first = email_changes
        .confirm(hash_first, now + 1)
        .await
        .expect("confirm first");
    assert_eq!(
        first,
        ConfirmEmailChangeOutcome::Changed {
            account_id: account.id,
            old_email: "original@example.com".to_owned(),
            new_email: "middle@example.com".to_owned(),
        }
    );

    let second = email_changes
        .confirm(hash_second, now + 2)
        .await
        .expect("confirm second");
    assert_eq!(
        second,
        ConfirmEmailChangeOutcome::Changed {
            account_id: account.id,
            old_email: "middle@example.com".to_owned(),
            new_email: "final@example.com".to_owned(),
        },
        "the second swap's 'old' email is what the account held just before IT, not the original"
    );
}

#[tokio::test]
async fn confirming_to_an_email_another_account_holds_is_taken_and_rolls_back_whole() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool);
    let now = now_millis();

    let account_a = accounts
        .create_verified_and_attach("a@example.com", now)
        .await
        .expect("create A");
    // Account B already holds the address A wants to move to.
    accounts
        .create_verified_and_attach("taken@example.com", now)
        .await
        .expect("create B holding the target email");

    let session_a = hash_token(b"a-session");
    sessions
        .insert(account_a.id, session_a, now, now + HOUR_MILLIS)
        .await
        .expect("insert A's session");

    let hash = hash_token(b"change-token-taken");
    email_changes
        .issue(account_a.id, "taken@example.com", hash, now + HOUR_MILLIS)
        .await
        .expect("issue token");

    let outcome = email_changes
        .confirm(hash, now + 1)
        .await
        .expect("confirm query must not surface a raw sqlx error");
    assert_eq!(outcome, ConfirmEmailChangeOutcome::EmailTaken);

    // The whole transaction rolled back: email unchanged, token unconsumed
    // (retryable after the address frees), sessions still live.
    let a_now = accounts
        .find_by_id(account_a.id)
        .await
        .expect("query")
        .expect("A exists");
    assert_eq!(a_now.email, "a@example.com", "A's email is unchanged");

    let token = email_changes
        .find(hash)
        .await
        .expect("find")
        .expect("token exists");
    assert_eq!(
        token.consumed_at_millis, None,
        "the token stays unconsumed so the user may retry"
    );

    let row = sessions
        .find(session_a)
        .await
        .expect("query")
        .expect("A's session exists");
    assert_eq!(
        session_verdict(&row.state, SESSION_IDLE_MILLIS, now + 2),
        SessionVerdict::Valid,
        "A's sessions must NOT be revoked on the rolled-back change"
    );
}

#[tokio::test]
async fn confirming_an_expired_email_change_token_is_not_consumable() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("old@example.com", now)
        .await
        .expect("create account");
    let hash = hash_token(b"change-token-expired");
    let expires_at = now + HOUR_MILLIS;
    email_changes
        .issue(account.id, "new@example.com", hash, expires_at)
        .await
        .expect("issue token");

    // Expiry is judged against the injected clock, never the DB clock: at
    // the boundary instant the token is refused (the clock-drift rule).
    let outcome = email_changes
        .confirm(hash, expires_at)
        .await
        .expect("confirm query");
    assert_eq!(outcome, ConfirmEmailChangeOutcome::NotConsumable);

    let a_now = accounts
        .find_by_id(account.id)
        .await
        .expect("query")
        .expect("account exists");
    assert_eq!(
        a_now.email, "old@example.com",
        "expired confirm changes nothing"
    );
}

// --- Account self-deletion + undelete window ---

const GRACE_MILLIS: u64 = 15 * 60 * 1000;

#[tokio::test]
async fn deleted_at_round_trips_through_both_read_paths() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let account = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    // A live account reads None on both paths.
    assert_eq!(
        account.deleted_at_millis, None,
        "fresh account is not pending"
    );
    assert_eq!(
        repo.find_by_id(account.id)
            .await
            .expect("query")
            .expect("account exists")
            .deleted_at_millis,
        None
    );

    repo.soft_delete(account.id, now)
        .await
        .expect("soft delete");

    assert_eq!(
        repo.find_by_id(account.id)
            .await
            .expect("query")
            .expect("account exists")
            .deleted_at_millis,
        Some(now),
        "find_by_id surfaces the pending instant"
    );
    assert_eq!(
        repo.find_by_email("op@example.com")
            .await
            .expect("query")
            .expect("account exists")
            .deleted_at_millis,
        Some(now),
        "find_by_email surfaces the pending instant"
    );
}

#[tokio::test]
async fn soft_delete_marks_pending_and_revokes_only_the_accounts_own_sessions() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool);
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    let bystander = accounts
        .create_verified_and_attach("bystander@example.com", now)
        .await
        .expect("create bystander");

    let session_a = hash_token(b"del-session-a");
    let session_b = hash_token(b"del-session-b");
    let bystander_session = hash_token(b"del-bystander-session");
    for (account_id, hash) in [
        (account.id, session_a),
        (account.id, session_b),
        (bystander.id, bystander_session),
    ] {
        sessions
            .insert(account_id, hash, now, now + HOUR_MILLIS)
            .await
            .expect("insert session");
    }

    let outcome = accounts
        .soft_delete(account.id, now + 1)
        .await
        .expect("soft delete");
    assert_eq!(outcome, SoftDeleteOutcome::Deleted);

    // Both of the account's sessions are revoked; the bystander's is not.
    for revoked in [session_a, session_b] {
        let row = sessions
            .find(revoked)
            .await
            .expect("query")
            .expect("session row exists");
        assert_eq!(
            session_verdict(&row.state, SESSION_IDLE_MILLIS, now + 2),
            SessionVerdict::Rejected,
            "the account's own sessions are all revoked on delete"
        );
    }
    let bystander_row = sessions
        .find(bystander_session)
        .await
        .expect("query")
        .expect("bystander session exists");
    assert_eq!(
        session_verdict(&bystander_row.state, SESSION_IDLE_MILLIS, now + 2),
        SessionVerdict::Valid,
        "a bystander's session must be untouched"
    );
}

#[tokio::test]
async fn soft_delete_is_idempotent_and_never_extends_the_grace_window() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let account = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");

    let first = repo
        .soft_delete(account.id, now)
        .await
        .expect("first delete");
    assert_eq!(first, SoftDeleteOutcome::Deleted);

    // A second delete much later must NOT move deleted_at — re-deleting an
    // already-pending account cannot extend the recovery window.
    let second = repo
        .soft_delete(account.id, now + 10 * GRACE_MILLIS)
        .await
        .expect("second delete");
    assert_eq!(second, SoftDeleteOutcome::AlreadyPending);
    assert_eq!(
        repo.find_by_id(account.id)
            .await
            .expect("query")
            .expect("account exists")
            .deleted_at_millis,
        Some(now),
        "the original pending instant is preserved (no window extension)"
    );
}

#[tokio::test]
async fn soft_delete_of_a_nonexistent_account_is_row_not_found() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);

    let result = repo.soft_delete(uuid::Uuid::now_v7(), now_millis()).await;
    assert!(
        matches!(result, Err(sqlx::Error::RowNotFound)),
        "an absent account must be distinguished from an idempotent no-op, got {result:?}"
    );
}

#[tokio::test]
async fn signing_back_in_clears_the_pending_deletion_and_preserves_state() {
    let (_container, pool) = migrated_pool().await;
    let repo = AccountRepo::new(pool);
    let now = now_millis();

    let account = repo
        .create_verified_and_attach("op@example.com", now)
        .await
        .expect("create account");
    repo.set_callsign(account.id, "W1AW", now)
        .await
        .expect("reserve callsign");
    repo.update_profile(account.id, &full_profile(), now)
        .await
        .expect("populate profile");

    repo.soft_delete(account.id, now)
        .await
        .expect("soft delete");
    assert_eq!(
        repo.find_by_id(account.id)
            .await
            .expect("query")
            .expect("account exists")
            .deleted_at_millis,
        Some(now)
    );

    // Undelete = the existing verify-and-attach sign-in path clears deleted_at.
    repo.verify_and_attach(account.id, now + 5_000)
        .await
        .expect("sign back in");

    let restored = repo
        .find_by_id(account.id)
        .await
        .expect("query")
        .expect("account exists");
    assert_eq!(
        restored.deleted_at_millis, None,
        "signing back in clears the pending deletion (undelete)"
    );
    assert_eq!(
        restored.callsign.as_deref(),
        Some("W1AW"),
        "callsign intact"
    );
    assert_eq!(
        restored.display_name.as_deref(),
        Some("Maria"),
        "profile intact"
    );
}

#[tokio::test]
async fn finalize_deletions_sweeps_only_past_window_accounts_and_cascades() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let email_changes = EmailChangeRepo::new(pool.clone());
    let consents = ConsentRepo::new(pool.clone());
    let now = now_millis();

    let live = accounts
        .create_verified_and_attach("live@example.com", now)
        .await
        .expect("create live");
    let in_window = accounts
        .create_verified_and_attach("inwindow@example.com", now)
        .await
        .expect("create in-window");
    let past_window = accounts
        .create_verified_and_attach("pastwindow@example.com", now)
        .await
        .expect("create past-window");

    // Soft-delete two: one still inside the window, one past it.
    accounts
        .soft_delete(in_window.id, now - GRACE_MILLIS / 2)
        .await
        .expect("soft delete in-window");
    accounts
        .soft_delete(past_window.id, now - GRACE_MILLIS - 1)
        .await
        .expect("soft delete past-window");

    // Give the past-window account a child row in each cascade table.
    sessions
        .insert(
            past_window.id,
            hash_token(b"pw-session"),
            now,
            now + HOUR_MILLIS,
        )
        .await
        .expect("seed session");
    email_changes
        .issue(
            past_window.id,
            "pw-new@example.com",
            hash_token(b"pw-change"),
            now + HOUR_MILLIS,
        )
        .await
        .expect("seed email-change token");
    consents
        .record(
            past_window.id,
            &ConsentRecord {
                terms_version: "2026-07-15".into(),
                consented_at_millis: now,
            },
        )
        .await
        .expect("seed consent");

    let finalized = accounts
        .finalize_deletions(now, GRACE_MILLIS)
        .await
        .expect("finalize sweep");
    assert_eq!(finalized, 1, "exactly the past-window account is finalized");

    assert!(
        accounts
            .find_by_id(past_window.id)
            .await
            .expect("query")
            .is_none(),
        "the past-window account is hard-deleted"
    );
    assert!(
        accounts
            .find_by_id(in_window.id)
            .await
            .expect("query")
            .is_some(),
        "the in-window account survives the sweep"
    );
    assert!(
        accounts.find_by_id(live.id).await.expect("query").is_some(),
        "the live account survives the sweep"
    );

    // The past-window account's children cascaded away.
    let orphan_sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE account_id = $1")
            .bind(past_window.id)
            .fetch_one(&pool)
            .await
            .expect("count sessions");
    let orphan_tokens: i64 =
        sqlx::query_scalar("SELECT count(*) FROM email_change_tokens WHERE account_id = $1")
            .bind(past_window.id)
            .fetch_one(&pool)
            .await
            .expect("count tokens");
    let orphan_consents: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_consents WHERE account_id = $1")
            .bind(past_window.id)
            .fetch_one(&pool)
            .await
            .expect("count consents");
    assert_eq!(
        (orphan_sessions, orphan_tokens, orphan_consents),
        (0, 0, 0),
        "the finalized account's child rows cascade away"
    );
}

#[tokio::test]
async fn finalize_account_hard_deletes_a_pending_account_but_guards_a_live_one() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let now = now_millis();

    let pending = accounts
        .create_verified_and_attach("pending@example.com", now)
        .await
        .expect("create pending");
    let live = accounts
        .create_verified_and_attach("live@example.com", now)
        .await
        .expect("create live");
    accounts
        .soft_delete(pending.id, now)
        .await
        .expect("soft delete");
    sessions
        .insert(
            pending.id,
            hash_token(b"fa-session"),
            now,
            now + HOUR_MILLIS,
        )
        .await
        .expect("seed session");

    // A live account is guarded: finalize_account refuses to delete it.
    assert!(
        !accounts
            .finalize_account(live.id)
            .await
            .expect("finalize live"),
        "finalize_account must not hard-delete a live (non-pending) account"
    );
    assert!(
        accounts.find_by_id(live.id).await.expect("query").is_some(),
        "the live account survives"
    );

    // The pending account is hard-deleted, its child session cascades away.
    assert!(
        accounts
            .finalize_account(pending.id)
            .await
            .expect("finalize pending"),
        "finalize_account deletes a pending account"
    );
    assert!(
        accounts
            .find_by_id(pending.id)
            .await
            .expect("query")
            .is_none(),
        "the pending account is gone"
    );
    let orphan_sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE account_id = $1")
            .bind(pending.id)
            .fetch_one(&pool)
            .await
            .expect("count sessions");
    assert_eq!(
        orphan_sessions, 0,
        "the finalized account's session cascades away"
    );
}

/// Proves the two independent finalize paths race harmlessly for real rather
/// than by assertion in prose: both call idempotent DELETEs guarded on
/// `deleted_at IS NOT NULL`. The background sweep (`finalize_deletions`) and
/// finalize-on-
/// access (`finalize_account`) fired concurrently at the SAME past-window
/// row must not error, double-cascade, or leave the row in an inconsistent
/// state. Bare autocommit DELETEs under Postgres's default READ COMMITTED
/// serialize at the row lock: whichever loses the race simply affects zero
/// rows, never an error.
#[tokio::test]
async fn concurrent_finalize_sweep_and_finalize_on_access_race_harmlessly() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let now = now_millis();

    let past_window = accounts
        .create_verified_and_attach("racer@example.com", now)
        .await
        .expect("create past-window account");
    accounts
        .soft_delete(past_window.id, now - GRACE_MILLIS - 1)
        .await
        .expect("soft delete past the window");
    sessions
        .insert(
            past_window.id,
            hash_token(b"racer-session"),
            now,
            now + HOUR_MILLIS,
        )
        .await
        .expect("seed session");

    let sweep_accounts = accounts.clone();
    let access_accounts = accounts.clone();
    let account_id = past_window.id;

    // Fire the bulk sweep and the single-row finalize-on-access concurrently
    // against the same row via separate pool connections.
    let (sweep_result, access_result) = tokio::join!(
        async move { sweep_accounts.finalize_deletions(now, GRACE_MILLIS).await },
        async move { access_accounts.finalize_account(account_id).await },
    );

    let finalized_count = sweep_result.expect("sweep must not error under the race");
    let access_deleted = access_result.expect("finalize_account must not error under the race");

    // Exactly one row existed and exactly one path can have removed it —
    // never both claiming success, never both claiming nothing happened.
    assert_eq!(
        (finalized_count, access_deleted),
        if access_deleted {
            (0, true)
        } else {
            (1, false)
        },
        "exactly one of the two racing finalize paths reports removing the row"
    );

    assert!(
        accounts
            .find_by_id(past_window.id)
            .await
            .expect("query")
            .is_none(),
        "the account is gone regardless of which path won the race"
    );
    let orphan_sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE account_id = $1")
            .bind(past_window.id)
            .fetch_one(&pool)
            .await
            .expect("count sessions");
    assert_eq!(
        orphan_sessions, 0,
        "the child session cascaded away exactly once, not left orphaned or double-deleted"
    );
}

/// "Disabling does not hard-delete data": neither finalize path may
/// hard-delete an account that is
/// `disabled_at`-set, even once its INDEPENDENT self-deletion grace window
/// has elapsed — otherwise an admin's disable is silently undone the moment
/// the unrelated deletion timer expires, erasing `disabled_at` and freeing
/// the email for a fresh signup with no `reenable` ever happening.
#[tokio::test]
async fn finalize_paths_refuse_a_disabled_account_even_past_its_grace_window() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let now = now_millis();

    let account = accounts
        .create_verified_and_attach("disabled-and-deleted@example.com", now)
        .await
        .expect("create account");
    accounts
        .disable(account.id, now, None)
        .await
        .expect("disable");
    accounts
        .soft_delete(account.id, now - GRACE_MILLIS - 1)
        .await
        .expect("soft delete past the window");

    assert!(
        !accounts
            .finalize_account(account.id)
            .await
            .expect("finalize_account must not error"),
        "finalize_account must refuse a disabled account, even past its grace window"
    );
    assert_eq!(
        accounts
            .finalize_deletions(now, GRACE_MILLIS)
            .await
            .expect("finalize_deletions must not error"),
        0,
        "finalize_deletions must sweep zero rows when the only eligible account is disabled"
    );

    let survivor = accounts
        .find_by_id(account.id)
        .await
        .expect("query")
        .expect("the disabled account survives both finalize paths");
    assert!(
        survivor.disabled_at_millis.is_some(),
        "disabled_at is untouched by the refused finalize attempts"
    );
}

// --- Net definitions -------------------------------------------

/// Seeds a verified account and returns its id (owner FK needs a real row).
async fn seed_account(pool: &PgPool, email: &str) -> uuid::Uuid {
    AccountRepo::new(pool.clone())
        .create_verified_and_attach(email, now_millis())
        .await
        .expect("seed account")
        .id
}

/// A fully-populated validated write shape for the repo tests.
/// The one connection every fixture net is born with: a
/// definition carries no connection fact of its own, so `create` takes the set
/// as an argument.
fn sample_connections() -> NetConnectionSet {
    rf_connections(Band::TwentyMeters, Mode::Ssb)
}

/// One HF way on `band`/`mode` — what a discovery filter test seeds a net with,
/// since band and mode are a connection's facts and discovery matches on the
/// connection set.
fn rf_connections(band: Band, mode: Mode) -> NetConnectionSet {
    NetConnectionSet::new(vec![NetConnection {
        id: uuid::Uuid::now_v7(),
        position: 0,
        kind: NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band,
            mode,
        },
    }])
    .expect("one connection is a valid set")
}

/// One EchoLink way in and nothing else — a net with no band and no mode,
/// the internet-only sibling of `rf_connections`.
fn echolink_connections() -> NetConnectionSet {
    NetConnectionSet::new(vec![NetConnection {
        id: uuid::Uuid::now_v7(),
        position: 0,
        kind: NetConnectionKind::EchoLink {
            node: "12345".to_owned(),
        },
    }])
    .expect("one connection is a valid set")
}

fn sample_fields() -> netroll_domain::net::validation::NetDefinitionFields {
    parse_net_definition_fields(RawNetDefinition {
        title: Some("Sunday Traffic Net".to_owned()),
        description: Some("Weekly NTS".to_owned()),
        country: Some("USA".to_owned()),
        state: Some("CT".to_owned()),
        grid: Some("fn31pr".to_owned()),
        net_category: Some("traffic".to_owned()),
        net_type: Some("open".to_owned()),
        expected_duration: Some("90".to_owned()),
        visibility: None,
    })
    .expect("valid sample fields")
}

#[tokio::test]
async fn create_inserts_definition_and_sole_owner_and_round_trips() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;

    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-create-roundtrip",
            now_millis(),
        )
        .await
        .expect("create definition");

    assert_eq!(created.definition_version, 1);
    assert_eq!(created.owner_account_ids, vec![owner]);
    assert_eq!(created.title, "Sunday Traffic Net");
    assert_eq!(created.grid.as_deref(), Some("FN31pr"));
    assert_eq!(created.expected_duration_minutes, Some(90));
    // The connection set the create was handed is the one the definition
    // carries — a definition has no RF fact anywhere else.
    assert_eq!(
        created.connections.connections()[0].kind,
        NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band: Band::TwentyMeters,
            mode: Mode::Ssb,
        }
    );
    // Visibility defaults to Listed (sample omits it); the passed link_token
    // is stored verbatim.
    assert_eq!(created.visibility, Visibility::Listed);
    assert_eq!(created.link_token, "tok-create-roundtrip");

    let fetched = repo
        .find_by_id(created.id)
        .await
        .expect("find")
        .expect("row present");
    assert_eq!(fetched, created, "find_by_id agrees with create");
}

#[tokio::test]
async fn create_persists_unlisted_visibility_and_its_token() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;

    let mut fields = sample_fields();
    fields.visibility = Visibility::Unlisted;
    let created = repo
        .create(
            &fields,
            &sample_connections(),
            owner,
            "tok-unlisted",
            now_millis(),
        )
        .await
        .expect("create unlisted");
    assert_eq!(created.visibility, Visibility::Unlisted);
    assert_eq!(created.link_token, "tok-unlisted");

    let fetched = repo
        .find_by_id(created.id)
        .await
        .expect("find")
        .expect("row present");
    assert_eq!(fetched.visibility, Visibility::Unlisted);
    assert_eq!(fetched.link_token, "tok-unlisted");
}

#[tokio::test]
async fn find_by_link_token_resolves_any_net_and_misses_return_none() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;

    let listed = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-listed",
            now_millis(),
        )
        .await
        .expect("create listed");
    let mut unlisted_fields = sample_fields();
    unlisted_fields.visibility = Visibility::Unlisted;
    let unlisted = repo
        .create(
            &unlisted_fields,
            &sample_connections(),
            owner,
            "tok-hidden",
            now_millis(),
        )
        .await
        .expect("create unlisted");

    // The token is a permalink for ANY net regardless of visibility.
    let by_listed = repo
        .find_by_link_token("tok-listed")
        .await
        .expect("query")
        .expect("listed net present");
    assert_eq!(by_listed.id, listed.id);
    assert_eq!(by_listed.owner_account_ids, vec![owner], "owners populated");

    let by_unlisted = repo
        .find_by_link_token("tok-hidden")
        .await
        .expect("query")
        .expect("unlisted net present");
    assert_eq!(by_unlisted.id, unlisted.id);

    // A nonexistent/garbage token resolves to None.
    assert!(
        repo.find_by_link_token("no-such-token")
            .await
            .expect("query")
            .is_none()
    );
}

#[tokio::test]
async fn list_discoverable_returns_only_listed_nets() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;

    let listed = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-disc-listed",
            now_millis(),
        )
        .await
        .expect("create listed");
    let mut unlisted_fields = sample_fields();
    unlisted_fields.visibility = Visibility::Unlisted;
    repo.create(
        &unlisted_fields,
        &sample_connections(),
        owner,
        "tok-disc-hidden",
        now_millis(),
    )
    .await
    .expect("create unlisted");

    let discoverable = repo.list_discoverable().await.expect("list discoverable");
    let ids: Vec<_> = discoverable.iter().map(|d| d.id).collect();
    assert_eq!(ids, vec![listed.id], "only the Listed net is discoverable");
    assert_eq!(
        discoverable[0].owner_account_ids,
        vec![owner],
        "owners populated on discovery rows"
    );
}

#[tokio::test]
async fn list_owned_returns_only_the_accounts_active_owned_nets() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let other = seed_account(&pool, "other@example.com").await;

    let owned = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-list-owned",
            now_millis(),
        )
        .await
        .expect("create owned");
    let others_net = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            other,
            "tok-list-others",
            now_millis(),
        )
        .await
        .expect("create another account's net");
    let archived = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-list-archived",
            now_millis(),
        )
        .await
        .expect("create net to archive");
    repo.archive(archived.id, now_millis())
        .await
        .expect("archive");

    let page = repo
        .list_owned_page(owner, 50, None)
        .await
        .expect("list owned page");
    assert!(page.next.is_none(), "one row is one page");
    let mine = page.rows;
    let ids: Vec<_> = mine.iter().map(|d| d.id).collect();
    assert_eq!(
        ids,
        vec![owned.id],
        "excludes another account's net and the archived one"
    );
    assert!(!ids.contains(&others_net.id));
    assert!(!ids.contains(&archived.id));
    assert_eq!(mine[0].owner_account_ids, vec![owner], "owners populated");
}

#[tokio::test]
async fn find_by_id_missing_is_none() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool);
    assert!(
        repo.find_by_id(uuid::Uuid::now_v7())
            .await
            .expect("query")
            .is_none()
    );
}

#[tokio::test]
async fn update_increments_version_and_leaves_owners_untouched() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-stable",
            now_millis(),
        )
        .await
        .expect("create");
    assert_eq!(created.definition_version, 1);
    assert_eq!(created.visibility, Visibility::Listed);

    // Flip visibility Listed -> Unlisted on edit; the version bumps but the
    // link_token stays byte-identical (a stable permalink).
    let mut edited = sample_fields();
    edited.title = "Monday Emergency Net".to_owned();
    edited.net_type = NetType::RollCall;
    edited.visibility = Visibility::Unlisted;
    let after_first = repo
        .update(created.id, &edited, now_millis())
        .await
        .expect("first edit");
    assert_eq!(after_first.definition_version, 2, "1 -> 2");
    assert_eq!(after_first.title, "Monday Emergency Net");
    assert_eq!(after_first.net_type, NetType::RollCall);
    assert_eq!(after_first.visibility, Visibility::Unlisted);
    assert_eq!(
        after_first.link_token, created.link_token,
        "link_token is stable across edits"
    );
    assert_eq!(
        after_first.owner_account_ids,
        vec![owner],
        "owners untouched by an edit"
    );

    // Flip back to Listed; still increments and still stable token.
    let mut back = sample_fields();
    back.visibility = Visibility::Listed;
    let after_second = repo
        .update(created.id, &back, now_millis())
        .await
        .expect("second edit");
    assert_eq!(after_second.definition_version, 3, "2 -> 3");
    assert_eq!(after_second.visibility, Visibility::Listed);
    assert_eq!(after_second.link_token, created.link_token);
}

#[tokio::test]
async fn update_missing_id_is_row_not_found() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool);
    let err = repo
        .update(uuid::Uuid::now_v7(), &sample_fields(), now_millis())
        .await
        .expect_err("a zero-row UPDATE must not be silent success");
    assert!(matches!(err, sqlx::Error::RowNotFound));
}

#[tokio::test]
async fn owner_account_ids_is_empty_for_a_missing_net() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool);
    assert_eq!(
        repo.owner_account_ids(uuid::Uuid::now_v7())
            .await
            .expect("query"),
        Vec::<uuid::Uuid>::new()
    );
}

#[tokio::test]
async fn archive_sets_archived_at_and_the_row_and_its_occurrences_survive() {
    // Owner-initiated delete becomes an archive. The
    // definition row and its occurrences survive for provenance; archive is
    // idempotent (a second archive is a no-op, not an error).
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();
    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-archive",
            now,
        )
        .await
        .expect("create");

    // Give it a future occurrence to prove archival never orphans history.
    let one_off = Schedule::OneOff {
        start_at_millis: now + 3 * 24 * 60 * 60 * 1000,
        timezone: chrono_tz::UTC,
    };
    schedules
        .set_schedule(created.id, &one_off, now)
        .await
        .expect("set one-off schedule");

    assert!(repo.archive(created.id, now).await.expect("archive"));

    let fetched = repo
        .find_by_id(created.id)
        .await
        .expect("query")
        .expect("the definition row survives archival");
    assert!(
        fetched.archived_at_millis.is_some(),
        "archived_at is set on the surviving row"
    );

    let occurrence_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_occurrences WHERE definition_id = $1")
            .bind(created.id)
            .fetch_one(&pool)
            .await
            .expect("count occurrences");
    assert_eq!(
        occurrence_rows, 1,
        "occurrences survive archival (provenance)"
    );

    // Idempotent: a second archive is a no-op (returns false), never an error.
    assert!(
        !repo.archive(created.id, now).await.expect("second archive"),
        "re-archiving an already-archived net is a no-op"
    );
}

// --- Multiple owners & ownership lifecycle ----------------------

#[tokio::test]
async fn find_by_callsign_resolves_the_holder_and_misses_are_none() {
    let (_container, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let now = now_millis();

    let holder = accounts
        .create_verified_and_attach("holder@example.com", now)
        .await
        .expect("create holder");
    accounts
        .set_callsign(holder.id, "W1AW", now)
        .await
        .expect("reserve callsign");
    // A callsign-less account exists too — it must never match.
    accounts
        .create_verified_and_attach("callsignless@example.com", now)
        .await
        .expect("create callsign-less account");

    let found = accounts
        .find_by_callsign("W1AW")
        .await
        .expect("query")
        .expect("holder resolves by callsign");
    assert_eq!(found.id, holder.id);
    assert_eq!(found.callsign.as_deref(), Some("W1AW"));

    assert!(
        accounts
            .find_by_callsign("K2XYZ")
            .await
            .expect("query")
            .is_none(),
        "a callsign no account holds resolves to None"
    );
}

#[tokio::test]
async fn add_owner_inserts_a_row_and_is_idempotent() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let creator = seed_account(&pool, "creator@example.com").await;
    let coowner = seed_account(&pool, "coowner@example.com").await;
    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            creator,
            "tok-add-owner",
            now_millis(),
        )
        .await
        .expect("create");

    repo.add_owner(created.id, coowner, now_millis(), usize::MAX)
        .await
        .expect("add co-owner");
    // A duplicate add is a no-op (the composite PK) — no error, no second row.
    repo.add_owner(created.id, coowner, now_millis(), usize::MAX)
        .await
        .expect("duplicate add is idempotent");

    let owner_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM net_definition_owners WHERE net_definition_id = $1",
    )
    .bind(created.id)
    .fetch_one(&pool)
    .await
    .expect("count owners");
    assert_eq!(owner_rows, 2, "creator + co-owner, no duplicate");
}

#[tokio::test]
async fn remove_owner_reports_whether_a_row_was_removed() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let creator = seed_account(&pool, "creator@example.com").await;
    let coowner = seed_account(&pool, "coowner@example.com").await;
    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            creator,
            "tok-remove-owner",
            now_millis(),
        )
        .await
        .expect("create");
    repo.add_owner(created.id, coowner, now_millis(), usize::MAX)
        .await
        .expect("add co-owner");

    assert_eq!(
        repo.remove_owner(created.id, coowner)
            .await
            .expect("remove co-owner"),
        RemoveOwnerOutcome::Removed,
        "removing a member reports Removed"
    );
    assert_eq!(
        repo.remove_owner(created.id, coowner)
            .await
            .expect("remove again"),
        RemoveOwnerOutcome::NotAMember,
        "removing a non-member reports NotAMember"
    );
    assert_eq!(
        repo.owner_account_ids(created.id).await.expect("owners"),
        vec![creator],
        "only the creator remains"
    );
}

#[tokio::test]
async fn remove_owner_refuses_the_sole_owner_atomically() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let sole = seed_account(&pool, "sole@example.com").await;
    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            sole,
            "tok-sole-owner",
            now_millis(),
        )
        .await
        .expect("create");

    assert_eq!(
        repo.remove_owner(created.id, sole)
            .await
            .expect("attempt to remove the sole owner"),
        RemoveOwnerOutcome::WouldOrphan,
        "the last-owner guard runs inside remove_owner itself, not just the caller"
    );
    assert_eq!(
        repo.owner_account_ids(created.id).await.expect("owners"),
        vec![sole],
        "the sole owner is untouched — the delete never ran"
    );
}

/// Reproduces the TOCTOU race a caller-side-only guard is vulnerable to: two
/// owners of the same net are removed CONCURRENTLY. If the last-owner check
/// and the delete were two independent steps (guard in the handler, delete
/// unconditional in the adapter — the shape this test is guarding against),
/// both racers could observe the same pre-delete 2-owner snapshot, both pass
/// their independent guard, and both deletes commit — leaving the net with
/// ZERO owners, despite the rule that a net can never be voluntarily orphaned
/// through the API. `remove_owner` closes this by making the guard-check and
/// the delete a single atomic, row-locked operation: the second racer blocks
/// until the first commits, then re-observes the POST-delete owner set.
#[tokio::test]
async fn concurrent_remove_owner_calls_never_leave_the_net_ownerless() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let a = seed_account(&pool, "a@example.com").await;
    let b = seed_account(&pool, "b@example.com").await;
    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            a,
            "tok-concurrent-remove",
            now_millis(),
        )
        .await
        .expect("create");
    repo.add_owner(created.id, b, now_millis(), usize::MAX)
        .await
        .expect("add second owner");

    let repo_a = repo.clone();
    let repo_b = repo.clone();
    let (result_a, result_b) = tokio::join!(
        async move { repo_a.remove_owner(created.id, a).await },
        async move { repo_b.remove_owner(created.id, b).await },
    );

    let outcomes = [
        result_a.expect("racing remove_owner must not surface a raw error"),
        result_b.expect("racing remove_owner must not surface a raw error"),
    ];
    let removed_count = outcomes
        .iter()
        .filter(|o| **o == RemoveOwnerOutcome::Removed)
        .count();
    let refused_count = outcomes
        .iter()
        .filter(|o| **o == RemoveOwnerOutcome::WouldOrphan)
        .count();
    assert_eq!(
        removed_count, 1,
        "exactly one racer removes an owner — the invariant the API guarantees"
    );
    assert_eq!(
        refused_count, 1,
        "the other racer is refused as would-be-last-owner, not silently allowed"
    );
    assert_eq!(
        repo.owner_account_ids(created.id)
            .await
            .expect("owners")
            .len(),
        1,
        "the net is left with exactly one owner — never zero"
    );
}

#[tokio::test]
async fn concurrent_add_owner_calls_never_exceed_the_cap() {
    // Mirrors `concurrent_remove_owner_calls_never_leave_the_net_ownerless`:
    // the one property the `add_owner` FOR UPDATE
    // transaction exists to guarantee — that a cap can't be exceeded by two
    // concurrent adds of DIFFERENT accounts each observing the same
    // pre-insert snapshot — was previously asserted only by a comment, never a
    // test.
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let creator = seed_account(&pool, "creator@example.com").await;
    let b = seed_account(&pool, "b@example.com").await;
    let c = seed_account(&pool, "c@example.com").await;
    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            creator,
            "tok-concurrent-add",
            now_millis(),
        )
        .await
        .expect("create");

    // Cap of 2: the net already has 1 owner (the creator), so exactly ONE of
    // the two concurrent adds of DISTINCT new accounts may succeed.
    let repo_b = repo.clone();
    let repo_c = repo.clone();
    let (result_b, result_c) = tokio::join!(
        async move { repo_b.add_owner(created.id, b, now_millis(), 2).await },
        async move { repo_c.add_owner(created.id, c, now_millis(), 2).await },
    );

    let outcomes = [
        result_b.expect("racing add_owner must not surface a raw error"),
        result_c.expect("racing add_owner must not surface a raw error"),
    ];
    let added_count = outcomes
        .iter()
        .filter(|o| **o == AddOwnerOutcome::Added)
        .count();
    let at_cap_count = outcomes
        .iter()
        .filter(|o| **o == AddOwnerOutcome::AtCap)
        .count();
    assert_eq!(
        added_count, 1,
        "exactly one racer is admitted — the cap the FOR UPDATE lock exists to enforce"
    );
    assert_eq!(
        at_cap_count, 1,
        "the other racer is refused at cap, not silently admitted past it"
    );
    assert_eq!(
        repo.owner_account_ids(created.id)
            .await
            .expect("owners")
            .len(),
        2,
        "the net never exceeds its 2-owner cap despite the race"
    );
}

#[tokio::test]
async fn owner_account_ids_ordering_has_a_stable_tiebreaker_on_a_shared_instant() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let a = seed_account(&pool, "a@example.com").await;
    let b = seed_account(&pool, "b@example.com").await;
    let at = now_millis();
    // Both owners share the SAME created_at — ordering must fall back to a
    // deterministic tiebreaker (account_id), not sort arbitrarily.
    let created = repo
        .create(&sample_fields(), &sample_connections(), a, "tok-order", at)
        .await
        .expect("create");
    repo.add_owner(created.id, b, at, usize::MAX)
        .await
        .expect("add second owner at the same instant");

    let mut expected = vec![a, b];
    expected.sort();
    let first = repo.owner_account_ids(created.id).await.expect("read 1");
    let second = repo.owner_account_ids(created.id).await.expect("read 2");
    assert_eq!(first, expected, "ordered by (created_at, account_id)");
    assert_eq!(first, second, "stable across repeated reads");
}

#[tokio::test]
async fn owners_with_callsign_returns_each_owner_and_its_callsign() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let accounts = AccountRepo::new(pool.clone());
    let now = now_millis();

    let a = accounts
        .create_verified_and_attach("a@example.com", now)
        .await
        .expect("create A");
    accounts
        .set_callsign(a.id, "W1AW", now)
        .await
        .expect("A callsign");
    let b = accounts
        .create_verified_and_attach("b@example.com", now)
        .await
        .expect("create B");
    accounts
        .set_callsign(b.id, "K2XYZ", now)
        .await
        .expect("B callsign");

    let created = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            a.id,
            "tok-owners-callsign",
            now,
        )
        .await
        .expect("create");
    repo.add_owner(created.id, b.id, now, usize::MAX)
        .await
        .expect("add B");

    let owners = repo
        .owners_with_callsign(created.id)
        .await
        .expect("owners with callsign");
    let by_id: std::collections::HashMap<_, _> = owners.into_iter().collect();
    assert_eq!(by_id.get(&a.id), Some(&Some("W1AW".to_owned())));
    assert_eq!(by_id.get(&b.id), Some(&Some("K2XYZ".to_owned())));
    assert_eq!(by_id.len(), 2);
}

#[tokio::test]
async fn archive_ownerless_archives_only_zero_owner_nets_and_is_idempotent() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    let owned = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-owned",
            now,
        )
        .await
        .expect("create owned");

    // With an owner present, nothing is archived.
    assert_eq!(
        repo.archive_ownerless(now).await.expect("archive sweep"),
        0,
        "an owned net is not archived"
    );
    assert!(
        repo.find_by_id(owned.id)
            .await
            .expect("query")
            .expect("present")
            .archived_at_millis
            .is_none()
    );

    // Empty the owner set the way the account-finalize cascade does.
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .expect("delete account (cascade empties owner set)");

    let archived = repo.archive_ownerless(now).await.expect("archive sweep");
    assert_eq!(archived, 1, "the now-ownerless net is archived");
    let after = repo
        .find_by_id(owned.id)
        .await
        .expect("query")
        .expect("net row survives (archived, not deleted)");
    assert_eq!(
        after.archived_at_millis,
        Some(now),
        "archived_at is set to the sweep instant"
    );

    // Idempotent: a second sweep archives nothing new and does not move the
    // already-set archived_at.
    assert_eq!(
        repo.archive_ownerless(now + HOUR_MILLIS)
            .await
            .expect("second sweep"),
        0,
        "a second sweep archives nothing"
    );
    assert_eq!(
        repo.find_by_id(owned.id)
            .await
            .expect("query")
            .expect("present")
            .archived_at_millis,
        Some(now),
        "the already-set archived_at is not moved"
    );
}

#[tokio::test]
async fn cascade_then_archive_solely_owned_archives_but_co_owned_survives() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let now = now_millis();

    // Solely-owned net: deleting the sole owner's account empties its owner
    // set; archive_ownerless then archives it.
    let sole = seed_account(&pool, "sole@example.com").await;
    let solely_owned = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            sole,
            "tok-sole",
            now,
        )
        .await
        .expect("create solely-owned");

    // Co-owned net: two owners; deleting ONE account leaves the other, so the
    // net must NOT be archived.
    let keep = seed_account(&pool, "keep@example.com").await;
    let leave = seed_account(&pool, "leave@example.com").await;
    let co_owned = repo
        .create(&sample_fields(), &sample_connections(), keep, "tok-co", now)
        .await
        .expect("create co-owned");
    repo.add_owner(co_owned.id, leave, now, usize::MAX)
        .await
        .expect("add second owner");

    sqlx::query("DELETE FROM accounts WHERE id = ANY($1)")
        .bind(vec![sole, leave])
        .execute(&pool)
        .await
        .expect("finalize both departing accounts");

    let archived = repo.archive_ownerless(now).await.expect("archive sweep");
    assert_eq!(archived, 1, "exactly the solely-owned net is archived");

    let solely = repo
        .find_by_id(solely_owned.id)
        .await
        .expect("query")
        .expect("solely-owned row survives");
    assert!(
        solely.archived_at_millis.is_some(),
        "the solely-owned net is archived"
    );

    let co = repo
        .find_by_id(co_owned.id)
        .await
        .expect("query")
        .expect("co-owned present");
    assert!(
        co.archived_at_millis.is_none(),
        "the co-owned net is NOT archived"
    );
    assert_eq!(
        co.owner_account_ids,
        vec![keep],
        "only the remaining owner is left"
    );
}

#[tokio::test]
async fn list_discoverable_excludes_archived_nets() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    let listed = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-disc-active",
            now,
        )
        .await
        .expect("create listed");
    // A second Listed net that becomes archived must drop out of discovery.
    let to_archive = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-disc-archived",
            now,
        )
        .await
        .expect("create second listed");
    sqlx::query("UPDATE net_definitions SET archived_at = now() WHERE id = $1")
        .bind(to_archive.id)
        .execute(&pool)
        .await
        .expect("archive the second net");

    let discoverable = repo.list_discoverable().await.expect("list discoverable");
    let ids: Vec<_> = discoverable.iter().map(|d| d.id).collect();
    assert_eq!(
        ids,
        vec![listed.id],
        "an archived Listed net is excluded from discovery"
    );
}

// --- Schedules & planned occurrences ----------------------------

const DAY_MILLIS: u64 = 24 * 60 * 60 * 1000;

fn weekly_utc(hour: u8) -> Schedule {
    Schedule::Recurring(RecurringSchedule {
        frequency: Frequency::Weekly,
        timezone: chrono_tz::UTC,
        hour,
        minute: 0,
        weekday: Some(chrono::Weekday::Mon),
        day_of_month: None,
    })
}

async fn occurrence_count(pool: &PgPool, definition_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_occurrences WHERE definition_id = $1")
        .bind(definition_id)
        .fetch_one(pool)
        .await
        .expect("count occurrences")
}

#[tokio::test]
async fn set_schedule_one_off_materializes_exactly_one_occurrence() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();
    let def = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-oneoff",
            now,
        )
        .await
        .expect("create");

    let start = now + 5 * DAY_MILLIS;
    let one_off = Schedule::OneOff {
        start_at_millis: start,
        timezone: chrono_tz::UTC,
    };
    schedules
        .set_schedule(def.id, &one_off, now)
        .await
        .expect("set one-off");

    let occ = schedules
        .upcoming_for_definition(def.id, now)
        .await
        .expect("list");
    assert_eq!(occ.len(), 1, "a one-off writes exactly one occurrence");
    assert_eq!(occ[0].scheduled_start_at_millis, start);
}

#[tokio::test]
async fn set_schedule_recurring_is_idempotent_on_repeat() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();
    let def = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-weekly",
            now,
        )
        .await
        .expect("create");

    schedules
        .set_schedule(def.id, &weekly_utc(20), now)
        .await
        .expect("set weekly");
    let first = occurrence_count(&pool, def.id).await;
    assert!(first > 0, "a weekly rule materializes horizon occurrences");

    // Re-calling with the SAME rule adds no duplicates (idempotency).
    schedules
        .set_schedule(def.id, &weekly_utc(20), now)
        .await
        .expect("re-set same weekly");
    assert_eq!(
        occurrence_count(&pool, def.id).await,
        first,
        "re-setting the same rule creates no duplicate occurrences"
    );
}

#[tokio::test]
async fn replace_schedule_regenerates_future_and_leaves_past_untouched() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();
    let def = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-replace",
            now,
        )
        .await
        .expect("create");

    // Seed a PAST occurrence directly (provenance) that no rule would produce.
    let past = now - 10 * DAY_MILLIS;
    sqlx::query(
        "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at) VALUES ($1,$2,$3)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(def.id)
    .bind(chrono::DateTime::from_timestamp_millis(past as i64).unwrap())
    .execute(&pool)
    .await
    .expect("seed past occurrence");

    schedules
        .set_schedule(def.id, &weekly_utc(20), now)
        .await
        .expect("set weekly 20:00");
    let hour20: Vec<u64> = schedules
        .upcoming_for_definition(def.id, now)
        .await
        .expect("list")
        .iter()
        .map(|o| o.scheduled_start_at_millis)
        .collect();

    // Replace with a different time-of-day: future occurrences at 20:00 must
    // be gone, replaced by the new rule's; the PAST occurrence survives.
    schedules
        .set_schedule(def.id, &weekly_utc(2), now)
        .await
        .expect("replace with weekly 02:00");
    let after = schedules
        .upcoming_for_definition(def.id, now)
        .await
        .expect("list");
    for o in &after {
        assert!(
            !hour20.contains(&o.scheduled_start_at_millis),
            "no stale 20:00 future occurrence remains after the rule changed"
        );
    }
    // The past occurrence is untouched (still present).
    let past_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM net_occurrences WHERE definition_id = $1 AND scheduled_start_at <= $2",
    )
    .bind(def.id)
    .bind(chrono::DateTime::from_timestamp_millis(now as i64).unwrap())
    .fetch_one(&pool)
    .await
    .expect("count past");
    assert_eq!(
        past_rows, 1,
        "the past occurrence is provenance — never rewritten"
    );
}

#[tokio::test]
async fn replace_schedule_across_a_genuine_recurrence_shape_change_has_no_dup_or_drop() {
    // The hardest case: not a no-op re-save or a same-shape time tweak
    // (covered above), but the recurrence RULE ITSELF changing shape —
    // weekly Monday -> weekly Wednesday, then weekly -> monthly. Every
    // future occurrence from the OLD shape must be gone, every occurrence
    // from the NEW shape must be present exactly once (no duplicates), and
    // the past occurrence must survive untouched throughout.
    use chrono::Datelike;

    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();
    let def = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-shape-change",
            now,
        )
        .await
        .expect("create");

    let past = now - 10 * DAY_MILLIS;
    sqlx::query(
        "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at) VALUES ($1,$2,$3)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(def.id)
    .bind(chrono::DateTime::from_timestamp_millis(past as i64).unwrap())
    .execute(&pool)
    .await
    .expect("seed past occurrence");

    let weekly_on = |weekday: chrono::Weekday| {
        Schedule::Recurring(RecurringSchedule {
            frequency: Frequency::Weekly,
            timezone: chrono_tz::UTC,
            hour: 20,
            minute: 0,
            weekday: Some(weekday),
            day_of_month: None,
        })
    };
    let monthly_on = |day_of_month: u8| {
        Schedule::Recurring(RecurringSchedule {
            frequency: Frequency::Monthly,
            timezone: chrono_tz::UTC,
            hour: 20,
            minute: 0,
            weekday: None,
            day_of_month: Some(day_of_month),
        })
    };

    // Step 1: weekly Monday.
    schedules
        .set_schedule(def.id, &weekly_on(chrono::Weekday::Mon), now)
        .await
        .expect("set weekly Monday");
    let monday_occurrences: Vec<u64> = schedules
        .upcoming_for_definition(def.id, now)
        .await
        .expect("list")
        .iter()
        .map(|o| o.scheduled_start_at_millis)
        .collect();
    assert!(!monday_occurrences.is_empty());
    for millis in &monday_occurrences {
        let weekday = chrono::DateTime::from_timestamp_millis(*millis as i64)
            .unwrap()
            .weekday();
        assert_eq!(
            weekday,
            chrono::Weekday::Mon,
            "every occurrence is a Monday"
        );
    }

    // Step 2: change the weekday (weekly Monday -> weekly Wednesday). The
    // rule's SHAPE is the same (still weekly) but the day filter differs —
    // every stored instant must move.
    schedules
        .set_schedule(def.id, &weekly_on(chrono::Weekday::Wed), now)
        .await
        .expect("replace with weekly Wednesday");
    let wednesday_occurrences: Vec<u64> = schedules
        .upcoming_for_definition(def.id, now)
        .await
        .expect("list")
        .iter()
        .map(|o| o.scheduled_start_at_millis)
        .collect();
    assert!(!wednesday_occurrences.is_empty());
    for millis in &wednesday_occurrences {
        assert!(
            !monday_occurrences.contains(millis),
            "no stale Monday occurrence survives the weekday change"
        );
        let weekday = chrono::DateTime::from_timestamp_millis(*millis as i64)
            .unwrap()
            .weekday();
        assert_eq!(
            weekday,
            chrono::Weekday::Wed,
            "every surviving occurrence is a Wednesday"
        );
    }
    // No duplicates: the independently-generated instant set for the new
    // rule matches exactly what set_schedule materialized (same length as a
    // dedup would report, since occurrences_between already dedups).
    let expected_wed = netroll_domain::net::schedule::occurrences_between(
        &weekly_on(chrono::Weekday::Wed),
        now,
        now + MATERIALIZATION_HORIZON_MILLIS,
    );
    assert_eq!(
        {
            let mut got = wednesday_occurrences.clone();
            got.sort_unstable();
            got
        },
        expected_wed,
        "the materialized set matches the new rule's instants exactly, once each"
    );

    // Step 3: change frequency entirely (weekly -> monthly). This flips the
    // stored columns' shape (weekday -> NULL, day_of_month populated) via the
    // ON CONFLICT DO UPDATE — every Wednesday occurrence must vanish and only
    // day-15-of-the-month occurrences remain.
    schedules
        .set_schedule(def.id, &monthly_on(15), now)
        .await
        .expect("replace with monthly day 15");
    let monthly_occurrences: Vec<u64> = schedules
        .upcoming_for_definition(def.id, now)
        .await
        .expect("list")
        .iter()
        .map(|o| o.scheduled_start_at_millis)
        .collect();
    assert!(!monthly_occurrences.is_empty());
    for millis in &monthly_occurrences {
        assert!(
            !wednesday_occurrences.contains(millis),
            "no stale weekly occurrence survives the frequency change"
        );
        let day = chrono::DateTime::from_timestamp_millis(*millis as i64)
            .unwrap()
            .day();
        assert_eq!(day, 15, "every surviving occurrence is the 15th");
    }
    let expected_monthly = netroll_domain::net::schedule::occurrences_between(
        &monthly_on(15),
        now,
        now + MATERIALIZATION_HORIZON_MILLIS,
    );
    assert_eq!(
        {
            let mut got = monthly_occurrences.clone();
            got.sort_unstable();
            got
        },
        expected_monthly,
        "no duplicates after the frequency change: exactly one row per new-rule instant"
    );

    // The past occurrence survived every replacement, untouched.
    let past_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM net_occurrences WHERE definition_id = $1 AND scheduled_start_at <= $2",
    )
    .bind(def.id)
    .bind(chrono::DateTime::from_timestamp_millis(now as i64).unwrap())
    .fetch_one(&pool)
    .await
    .expect("count past");
    assert_eq!(
        past_rows, 1,
        "the past occurrence is provenance — survives every rule-shape change"
    );
}

#[tokio::test]
async fn clear_schedule_removes_row_and_future_occurrences_only() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();
    let def = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-clear",
            now,
        )
        .await
        .expect("create");

    // A past occurrence (provenance) and a future rule.
    let past = now - 10 * DAY_MILLIS;
    sqlx::query(
        "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at) VALUES ($1,$2,$3)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(def.id)
    .bind(chrono::DateTime::from_timestamp_millis(past as i64).unwrap())
    .execute(&pool)
    .await
    .expect("seed past");
    schedules
        .set_schedule(def.id, &weekly_utc(20), now)
        .await
        .expect("set weekly");

    schedules.clear_schedule(def.id, now).await.expect("clear");

    let schedule_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_schedules WHERE definition_id = $1")
            .bind(def.id)
            .fetch_one(&pool)
            .await
            .expect("count schedule");
    assert_eq!(schedule_rows, 0, "the schedule row is removed");

    assert_eq!(
        schedules
            .upcoming_for_definition(def.id, now)
            .await
            .expect("list")
            .len(),
        0,
        "future occurrences are cleared"
    );
    assert_eq!(
        occurrence_count(&pool, def.id).await,
        1,
        "the past occurrence survives"
    );
}

#[tokio::test]
async fn spawn_due_occurrences_is_idempotent_and_advances_the_horizon() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();
    let def = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-spawn",
            now,
        )
        .await
        .expect("create");

    // set_schedule materializes (now, now + HORIZON] at set-time.
    let horizon = MATERIALIZATION_HORIZON_MILLIS;
    schedules
        .set_schedule(def.id, &weekly_utc(20), now)
        .await
        .expect("set weekly");
    let initial = occurrence_count(&pool, def.id).await;

    // Spawning over the SAME window inserts nothing (idempotent).
    let inserted = schedules
        .spawn_due_occurrences(now, horizon)
        .await
        .expect("spawn");
    assert_eq!(
        inserted, 0,
        "spawning the already-materialized window inserts nothing"
    );

    // Advancing `now` beyond the initial window's far edge extends the horizon
    // — occurrences in (now+HORIZON, later+HORIZON] are newly materialized
    // (injected clock — no sleeps).
    let later = now + horizon + 20 * DAY_MILLIS; // past the initial window's far edge
    let inserted_later = schedules
        .spawn_due_occurrences(later, horizon)
        .await
        .expect("spawn later");
    assert!(
        inserted_later > 0,
        "advancing the clock materializes new occurrences"
    );
    assert!(occurrence_count(&pool, def.id).await > initial);

    // Repeating at the same later `now` is idempotent again.
    assert_eq!(
        schedules
            .spawn_due_occurrences(later, horizon)
            .await
            .expect("spawn later repeat"),
        0,
        "the repeat spawn inserts nothing"
    );
}

#[tokio::test]
async fn spawn_due_occurrences_skips_archived_and_one_off() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();
    let horizon = 60 * DAY_MILLIS;

    // An archived net with a recurring schedule: spawn must skip it.
    let archived = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-arch-recurring",
            now,
        )
        .await
        .expect("create archived");
    schedules
        .set_schedule(archived.id, &weekly_utc(20), now)
        .await
        .expect("schedule archived net");
    // Remove its future occurrences so spawn would re-create them IF it didn't skip.
    schedules
        .clear_schedule(archived.id, now)
        .await
        .expect("clear");
    schedules
        .set_schedule(archived.id, &weekly_utc(20), now)
        .await
        .expect("re-set");
    sqlx::query("DELETE FROM net_occurrences WHERE definition_id = $1")
        .bind(archived.id)
        .execute(&pool)
        .await
        .expect("wipe occurrences");
    repo.archive(archived.id, now).await.expect("archive");

    // A one-off net: spawn must skip it (fully materialized at set-time).
    let oneoff = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-oneoff-skip",
            now,
        )
        .await
        .expect("create one-off");
    schedules
        .set_schedule(
            oneoff.id,
            &Schedule::OneOff {
                start_at_millis: now + 5 * DAY_MILLIS,
                timezone: chrono_tz::UTC,
            },
            now,
        )
        .await
        .expect("set one-off");

    let inserted = schedules
        .spawn_due_occurrences(now, horizon)
        .await
        .expect("spawn");
    assert_eq!(
        occurrence_count(&pool, archived.id).await,
        0,
        "an archived net's occurrences are never spawned"
    );
    // The one-off already had its single occurrence; spawn added none for it.
    assert_eq!(
        occurrence_count(&pool, oneoff.id).await,
        1,
        "a one-off is not re-materialized by the spawn"
    );
    assert_eq!(
        inserted, 0,
        "no non-archived recurring net exists in this fixture to spawn for"
    );
}

#[tokio::test]
async fn list_upcoming_occurrences_excludes_unlisted_and_archived() {
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    let listed = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-up-listed",
            now,
        )
        .await
        .expect("create listed");
    let mut unlisted_fields = sample_fields();
    unlisted_fields.visibility = Visibility::Unlisted;
    let unlisted = repo
        .create(
            &unlisted_fields,
            &sample_connections(),
            owner,
            "tok-up-unlisted",
            now,
        )
        .await
        .expect("create unlisted");
    let archived = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-up-archived",
            now,
        )
        .await
        .expect("create to-archive");

    for d in [listed.id, unlisted.id, archived.id] {
        schedules
            .set_schedule(d, &weekly_utc(20), now)
            .await
            .expect("schedule");
    }
    repo.archive(archived.id, now).await.expect("archive");

    let upcoming = schedules
        .list_upcoming_occurrences(now, 500)
        .await
        .expect("list upcoming");
    let def_ids: std::collections::HashSet<_> = upcoming.iter().map(|o| o.definition_id).collect();
    assert!(
        def_ids.contains(&listed.id),
        "listed net's occurrences appear"
    );
    assert!(
        !def_ids.contains(&unlisted.id),
        "an unlisted net's occurrences never appear in discovery"
    );
    assert!(
        !def_ids.contains(&archived.id),
        "an archived net's occurrences never appear in discovery"
    );
    // Ascending by scheduled_start_at.
    for w in upcoming.windows(2) {
        assert!(w[0].scheduled_start_at_millis <= w[1].scheduled_start_at_millis);
    }
}

// --- Public discovery read model --------------------------------

/// Creates a Listed net from `fields` and materializes ONE future occurrence
/// at `now + start_offset_millis`, returning the definition id.
#[expect(
    clippy::too_many_arguments,
    reason = "a test seeding helper that names every fact a discovery row is built from; the \
              eighth argument is the connection set, a separate input"
)]
async fn seed_upcoming(
    defs: &NetDefinitionRepo,
    schedules: &ScheduleRepo,
    owner: uuid::Uuid,
    token: &str,
    fields: &NetDefinitionFields,
    connections: &NetConnectionSet,
    now: u64,
    start_offset_millis: u64,
) -> uuid::Uuid {
    let def = defs
        .create(fields, connections, owner, token, now)
        .await
        .expect("create definition");
    schedules
        .set_schedule(
            def.id,
            &Schedule::OneOff {
                start_at_millis: now + start_offset_millis,
                timezone: chrono_tz::UTC,
            },
            now,
        )
        .await
        .expect("materialize one-off occurrence");
    def.id
}

/// `sample_fields()` with `mutate` applied — a terse per-test field builder.
fn fields_with(mutate: impl FnOnce(&mut NetDefinitionFields)) -> NetDefinitionFields {
    let mut fields = sample_fields();
    mutate(&mut fields);
    fields
}

#[tokio::test]
async fn list_discovery_upcoming_joins_definition_fields_and_orders_by_time_ascending() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let discovery = DiscoveryRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    // Created later-first to prove ordering is by time, not insertion order.
    let later = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-disc-later",
        &fields_with(|f| f.title = "Later Net".to_owned()),
        &sample_connections(),
        now,
        5 * DAY_MILLIS,
    )
    .await;
    let sooner = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-disc-sooner",
        &fields_with(|f| f.title = "Sooner Net".to_owned()),
        &sample_connections(),
        now,
        2 * DAY_MILLIS,
    )
    .await;

    let rows = discovery
        .list_discovery_upcoming(&DiscoveryQuery::default(), now, 100)
        .await
        .expect("list discovery upcoming")
        .rows;

    let def_ids: Vec<_> = rows.iter().map(|r| r.definition_id).collect();
    assert_eq!(
        def_ids,
        vec![sooner, later],
        "default sort is soonest scheduled_start_at first"
    );

    // The join carries the definition's display fields onto the row, and the
    // batched connection load carries its ways in; the flat band, mode and
    // frequency the row used to carry are retired.
    let sooner_row = &rows[0];
    assert_eq!(sooner_row.title, "Sooner Net");
    assert_eq!(sooner_row.net_category, NetCategory::Traffic);
    assert_eq!(
        sooner_row
            .connections
            .as_ref()
            .expect("the upcoming read attaches the definition's connections")
            .connections()[0]
            .kind,
        NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band: Band::TwentyMeters,
            mode: Mode::Ssb,
        }
    );
    assert_eq!(sooner_row.scheduled_start_at_millis, now + 2 * DAY_MILLIS);
    assert!(sooner_row.definition_version >= 1);
    // The token rides the same joined row, so the card can link to
    // `/nets/t/{token}` without a second read. Asserted here as well as at the
    // HTTP layer because a hardcoded stub would keep that layer green alone
    // (review found exactly that gap on the field it added).
    assert_eq!(sooner_row.link_token, "tok-disc-sooner");
}

#[tokio::test]
async fn list_active_now_carries_the_definitions_link_token_on_the_live_read() {
    // The upcoming read has an adapter-level token assertion explaining why
    // the HTTP layer alone is not enough — and the LIVE read, which this file's own
    // module doc and `api_discovery.rs` both call a SECOND, INDEPENDENT SQL
    // statement, got none. This is that fence: the token must come off
    // `net_definitions` on this statement too, not off the session snapshot,
    // because a token is a property of the definition with no frozen-at-start
    // meaning.
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "active-now-actor@example.com").await;
    let (_sessions, _log, _session) =
        start_live_session(&pool, "active-now@example.com", actor).await;

    let bounded = DiscoveryRepo::new(pool.clone())
        .list_active_now(100)
        .await
        .expect("list active now");

    assert_eq!(
        bounded.rows.len(),
        1,
        "the live Listed net is the only card"
    );
    assert_eq!(
        bounded.rows[0].link_token, "tok-active-now@example.com",
        "the live read projects d.link_token, the token start_live_session issued"
    );
}

#[tokio::test]
async fn list_discovery_upcoming_excludes_unlisted_and_archived() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let discovery = DiscoveryRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    let listed = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-disc-listed",
        &sample_fields(),
        &sample_connections(),
        now,
        DAY_MILLIS,
    )
    .await;
    let unlisted = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-disc-unlisted",
        &fields_with(|f| f.visibility = Visibility::Unlisted),
        &sample_connections(),
        now,
        DAY_MILLIS,
    )
    .await;
    let archived = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-disc-archived",
        &sample_fields(),
        &sample_connections(),
        now,
        DAY_MILLIS,
    )
    .await;
    defs.archive(archived, now).await.expect("archive");

    let rows = discovery
        .list_discovery_upcoming(&DiscoveryQuery::default(), now, 100)
        .await
        .expect("list discovery upcoming")
        .rows;
    let def_ids: std::collections::HashSet<_> = rows.iter().map(|r| r.definition_id).collect();

    assert!(def_ids.contains(&listed), "the Listed net appears");
    assert!(
        !def_ids.contains(&unlisted),
        "an Unlisted net never appears in discovery"
    );
    assert!(
        !def_ids.contains(&archived),
        "an archived net never appears in discovery"
    );
}

#[tokio::test]
async fn list_discovery_upcoming_past_occurrences_are_excluded() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let discovery = DiscoveryRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    let def = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-disc-past",
            now,
        )
        .await
        .expect("create");
    // A past occurrence (provenance) seeded directly — it must not surface.
    let past = now - 3 * DAY_MILLIS;
    sqlx::query(
        "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at) VALUES ($1,$2,$3)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(def.id)
    .bind(chrono::DateTime::from_timestamp_millis(past as i64).unwrap())
    .execute(&pool)
    .await
    .expect("seed past occurrence");

    let rows = discovery
        .list_discovery_upcoming(&DiscoveryQuery::default(), now, 100)
        .await
        .expect("list")
        .rows;
    assert!(
        rows.is_empty(),
        "only occurrences with scheduled_start_at > now surface"
    );
}

#[tokio::test]
async fn list_discovery_upcoming_filters_narrow_the_set() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let discovery = DiscoveryRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    // A 20m/ssb/traffic/open net in CT titled "Sunday Traffic".
    let a = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-f-a",
        &fields_with(|f| {
            f.title = "Sunday Traffic".to_owned();
            f.net_category = NetCategory::Traffic;
            f.net_type = NetType::Open;
            f.state = Some("CT".to_owned());
        }),
        &rf_connections(Band::TwentyMeters, Mode::Ssb),
        now,
        DAY_MILLIS,
    )
    .await;
    // A 2m/fm/emergency/roll-call net in MA titled "Monday Emergency".
    let b = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-f-b",
        &fields_with(|f| {
            f.title = "Monday Emergency".to_owned();
            f.net_category = NetCategory::Emergency;
            f.net_type = NetType::RollCall;
            f.state = Some("MA".to_owned());
        }),
        &rf_connections(Band::TwoMeters, Mode::Fm),
        now,
        2 * DAY_MILLIS,
    )
    .await;
    // An EchoLink-only club roll-call in VT titled "Tuesday EchoLink" — no band,
    // no mode, so no filter above can reach it.
    let c = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-f-c",
        &fields_with(|f| {
            f.title = "Tuesday EchoLink".to_owned();
            f.net_category = NetCategory::Club;
            f.net_type = NetType::RollCall;
            f.state = Some("VT".to_owned());
        }),
        &echolink_connections(),
        now,
        3 * DAY_MILLIS,
    )
    .await;

    let only = |rows: &[netroll_adapters::pg::discovery::DiscoveryUpcomingRow], id: uuid::Uuid| {
        let ids: Vec<_> = rows.iter().map(|r| r.definition_id).collect();
        assert_eq!(ids, vec![id], "filter must return exactly the matching net");
    };
    let ids_of = |rows: &[netroll_adapters::pg::discovery::DiscoveryUpcomingRow]| {
        rows.iter().map(|r| r.definition_id).collect::<Vec<_>>()
    };
    let kind_query = |kind: &str| DiscoveryQuery {
        filters: DiscoveryFilters {
            kind: Some(ConnectionKindFilter::try_from(kind).expect("a filterable kind")),
            ..Default::default()
        },
        sort: DiscoverySort::Time,
        sort_unavailable: None,
    };

    // Empty filter returns all three.
    let all = discovery
        .list_discovery_upcoming(&DiscoveryQuery::default(), now, 100)
        .await
        .expect("all")
        .rows;
    assert_eq!(all.len(), 3, "an empty filter returns all upcoming");

    // Connection kind, in BOTH directions: the EchoLink-only
    // net is the ONLY answer to `echolink`, is ABSENT from `hf`, and `repeater`
    // — a kind no seeded net has — narrows to nothing. A predicate that is
    // accidentally a no-op passes the first half alone.
    only(
        &discovery
            .list_discovery_upcoming(&kind_query("echolink"), now, 100)
            .await
            .expect("kind filter")
            .rows,
        c,
    );
    assert_eq!(
        ids_of(
            &discovery
                .list_discovery_upcoming(&kind_query("hf"), now, 100)
                .await
                .expect("hf kind filter")
                .rows
        ),
        vec![a, b],
        "both RF nets are HF ways; the EchoLink net is not"
    );
    assert!(
        discovery
            .list_discovery_upcoming(&kind_query("repeater"), now, 100)
            .await
            .expect("repeater kind filter")
            .rows
            .is_empty(),
        "no seeded net has a repeater way"
    );

    // Band.
    only(
        &discovery
            .list_discovery_upcoming(
                &DiscoveryQuery {
                    filters: DiscoveryFilters {
                        band: Some(Band::TwoMeters),
                        ..Default::default()
                    },
                    sort: DiscoverySort::Time,
                    sort_unavailable: None,
                },
                now,
                100,
            )
            .await
            .expect("band filter")
            .rows,
        b,
    );

    // Mode.
    only(
        &discovery
            .list_discovery_upcoming(
                &DiscoveryQuery {
                    filters: DiscoveryFilters {
                        mode: Some(Mode::Ssb),
                        ..Default::default()
                    },
                    sort: DiscoverySort::Time,
                    sort_unavailable: None,
                },
                now,
                100,
            )
            .await
            .expect("mode filter")
            .rows,
        a,
    );

    // Category.
    only(
        &discovery
            .list_discovery_upcoming(
                &DiscoveryQuery {
                    filters: DiscoveryFilters {
                        net_category: Some(NetCategory::Emergency),
                        ..Default::default()
                    },
                    sort: DiscoverySort::Time,
                    sort_unavailable: None,
                },
                now,
                100,
            )
            .await
            .expect("category filter")
            .rows,
        b,
    );

    // Net type.
    only(
        &discovery
            .list_discovery_upcoming(
                &DiscoveryQuery {
                    filters: DiscoveryFilters {
                        net_type: Some(NetType::Open),
                        ..Default::default()
                    },
                    sort: DiscoverySort::Time,
                    sort_unavailable: None,
                },
                now,
                100,
            )
            .await
            .expect("type filter")
            .rows,
        a,
    );

    // Geography (state).
    only(
        &discovery
            .list_discovery_upcoming(
                &DiscoveryQuery {
                    filters: DiscoveryFilters {
                        state: Some("MA".to_owned()),
                        ..Default::default()
                    },
                    sort: DiscoverySort::Time,
                    sort_unavailable: None,
                },
                now,
                100,
            )
            .await
            .expect("state filter")
            .rows,
        b,
    );

    // Case-insensitive substring on title.
    only(
        &discovery
            .list_discovery_upcoming(
                &DiscoveryQuery {
                    filters: DiscoveryFilters {
                        name: Some("sunday".to_owned()),
                        ..Default::default()
                    },
                    sort: DiscoverySort::Time,
                    sort_unavailable: None,
                },
                now,
                100,
            )
            .await
            .expect("name filter")
            .rows,
        a,
    );

    // Combined filters AND together: band=2m AND mode=fm matches only B.
    only(
        &discovery
            .list_discovery_upcoming(
                &DiscoveryQuery {
                    filters: DiscoveryFilters {
                        band: Some(Band::TwoMeters),
                        mode: Some(Mode::Fm),
                        ..Default::default()
                    },
                    sort: DiscoverySort::Time,
                    sort_unavailable: None,
                },
                now,
                100,
            )
            .await
            .expect("combined filter")
            .rows,
        b,
    );

    // Contradictory combined filters (band=2m AND mode=ssb) match nothing.
    let none = discovery
        .list_discovery_upcoming(
            &DiscoveryQuery {
                filters: DiscoveryFilters {
                    band: Some(Band::TwoMeters),
                    mode: Some(Mode::Ssb),
                    ..Default::default()
                },
                sort: DiscoverySort::Time,
                sort_unavailable: None,
            },
            now,
            100,
        )
        .await
        .expect("contradictory filter")
        .rows;
    assert!(
        none.is_empty(),
        "AND of contradictory filters returns nothing"
    );
}

#[tokio::test]
async fn list_discovery_upcoming_sort_by_name_reorders_deterministically() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let discovery = DiscoveryRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    // "Zulu" is sooner in time; "Alpha" is later. Time sort => Zulu first;
    // name sort => Alpha first — the reorder proves the sort key is honored.
    let zulu = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-s-zulu",
        &fields_with(|f| f.title = "Zulu Net".to_owned()),
        &sample_connections(),
        now,
        DAY_MILLIS,
    )
    .await;
    let alpha = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-s-alpha",
        &fields_with(|f| f.title = "Alpha Net".to_owned()),
        &sample_connections(),
        now,
        2 * DAY_MILLIS,
    )
    .await;

    let by_time: Vec<_> = discovery
        .list_discovery_upcoming(&DiscoveryQuery::default(), now, 100)
        .await
        .expect("time sort")
        .rows
        .iter()
        .map(|r| r.definition_id)
        .collect();
    assert_eq!(by_time, vec![zulu, alpha], "time sort: soonest first");

    let by_name: Vec<_> = discovery
        .list_discovery_upcoming(
            &DiscoveryQuery {
                filters: DiscoveryFilters::default(),
                sort: DiscoverySort::Name,
                sort_unavailable: None,
            },
            now,
            100,
        )
        .await
        .expect("name sort")
        .rows
        .iter()
        .map(|r| r.definition_id)
        .collect();
    assert_eq!(
        by_name,
        vec![alpha, zulu],
        "name sort: 'Alpha' before 'Zulu' regardless of time"
    );
}

#[tokio::test]
async fn list_discovery_upcoming_respects_the_limit_cap() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let discovery = DiscoveryRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    for i in 0..3u64 {
        seed_upcoming(
            &defs,
            &schedules,
            owner,
            &format!("tok-lim-{i}"),
            &sample_fields(),
            &sample_connections(),
            now,
            (i + 1) * DAY_MILLIS,
        )
        .await;
    }

    let capped = discovery
        .list_discovery_upcoming(&DiscoveryQuery::default(), now, 2)
        .await
        .expect("capped list");
    assert_eq!(capped.rows.len(), 2, "the result set honors the limit cap");
    // The cap and the STATEMENT that it bit are two facts, and only the first
    // was asserted here. Without this line
    // `truncated` can be hardcoded `false` and every test in this file stays
    // green — the flag is fenced only at the HTTP layer, where each fixture
    // costs 200+ seeded rows to reach a one-line adapter predicate.
    assert!(
        capped.truncated,
        "three matching rows served at a bound of two is a CUT, and the row \
         set alone cannot say so"
    );

    // The other side of the boundary, and the reason the split is `>` and not
    // `==`: exactly the bound is a COMPLETE answer.
    let exact = discovery
        .list_discovery_upcoming(&DiscoveryQuery::default(), now, 3)
        .await
        .expect("exact list");
    assert_eq!(exact.rows.len(), 3);
    assert!(
        !exact.truncated,
        "three matching rows served at a bound of three is complete"
    );
}

/// `country`/`state`/`grid` are free-text
/// (not enum) filters, so an exact case-sensitive `=` silently returns an
/// empty set for any case mismatch — a false negative a visitor reading the
/// wire copy verbatim (e.g. "CT" from a definition body) would never trigger,
/// but a visitor typing casually ("ct") would. Asserts country/state/grid
/// narrow the set (country/grid had no direct coverage before this) AND that
/// the match is case-insensitive, mirroring the `name` filter's posture.
#[tokio::test]
async fn list_discovery_upcoming_geography_filters_are_case_insensitive_and_narrow_the_set() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let schedules = ScheduleRepo::new(pool.clone());
    let discovery = DiscoveryRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner@example.com").await;
    let now = now_millis();

    let ct_net = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-geo-ct",
        &fields_with(|f| {
            f.title = "Connecticut Net".to_owned();
            f.country = Some("USA".to_owned());
            f.state = Some("CT".to_owned());
            f.grid = Some(netroll_domain::profile::parse_grid("FN31").expect("valid grid"));
        }),
        &sample_connections(),
        now,
        DAY_MILLIS,
    )
    .await;
    let ma_net = seed_upcoming(
        &defs,
        &schedules,
        owner,
        "tok-geo-ma",
        &fields_with(|f| {
            f.title = "Massachusetts Net".to_owned();
            f.country = Some("USA".to_owned());
            f.state = Some("MA".to_owned());
            f.grid = Some(netroll_domain::profile::parse_grid("FN42").expect("valid grid"));
        }),
        &sample_connections(),
        now,
        2 * DAY_MILLIS,
    )
    .await;

    let by = |filters: DiscoveryFilters| DiscoveryQuery {
        filters,
        sort: DiscoverySort::Time,
        sort_unavailable: None,
    };
    let only = |rows: &[netroll_adapters::pg::discovery::DiscoveryUpcomingRow], id: uuid::Uuid| {
        let ids: Vec<_> = rows.iter().map(|r| r.definition_id).collect();
        assert_eq!(ids, vec![id], "geography filter must match exactly one net");
    };

    // Country narrows nothing here (both USA) but must not exclude either —
    // proves `country` is wired, not silently ignored.
    let both = discovery
        .list_discovery_upcoming(
            &by(DiscoveryFilters {
                country: Some("USA".to_owned()),
                ..Default::default()
            }),
            now,
            100,
        )
        .await
        .expect("country filter")
        .rows;
    assert_eq!(both.len(), 2, "country=USA matches both nets");

    // State, exact case, narrows to one (direct coverage — was untested).
    only(
        &discovery
            .list_discovery_upcoming(
                &by(DiscoveryFilters {
                    state: Some("MA".to_owned()),
                    ..Default::default()
                }),
                now,
                100,
            )
            .await
            .expect("state filter")
            .rows,
        ma_net,
    );

    // State, mismatched case ("ct" vs stored "CT") still narrows to one.
    only(
        &discovery
            .list_discovery_upcoming(
                &by(DiscoveryFilters {
                    state: Some("ct".to_owned()),
                    ..Default::default()
                }),
                now,
                100,
            )
            .await
            .expect("case-insensitive state filter")
            .rows,
        ct_net,
    );

    // Grid, direct coverage (was untested), mismatched case ("fn42" vs the
    // canonically-stored "FN42") still narrows to one.
    only(
        &discovery
            .list_discovery_upcoming(
                &by(DiscoveryFilters {
                    grid: Some("fn42".to_owned()),
                    ..Default::default()
                }),
                now,
                100,
            )
            .await
            .expect("case-insensitive grid filter")
            .rows,
        ma_net,
    );
}

// --- Favorites / "My Nets" -------------------------------------

/// Creates a net owned by `owner` and returns its id, letting a test vary
/// visibility without repeating the `create` boilerplate.
async fn seed_net(
    defs: &NetDefinitionRepo,
    owner: uuid::Uuid,
    link_token: &str,
    visibility: Visibility,
) -> uuid::Uuid {
    let mut fields = sample_fields();
    fields.visibility = visibility;
    defs.create(
        &fields,
        &sample_connections(),
        owner,
        link_token,
        now_millis(),
    )
    .await
    .expect("create net")
    .id
}

async fn favorite_row_count(pool: &PgPool, account_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_favorites WHERE account_id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("count favorites")
}

#[tokio::test]
async fn add_favorite_is_idempotent_no_duplicate_row_no_error() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let favorites = FavoritesRepo::new(pool.clone());
    let account = seed_account(&pool, "fav@example.com").await;
    let net = seed_net(&defs, account, "tok-idem", Visibility::Listed).await;

    favorites
        .add(account, net, now_millis())
        .await
        .expect("first favorite");
    favorites
        .add(account, net, now_millis())
        .await
        .expect("re-favoriting is a no-op success, never a duplicate or error");

    assert_eq!(
        favorite_row_count(&pool, account).await,
        1,
        "favoriting twice leaves exactly one row (composite PK idempotency)"
    );
}

#[tokio::test]
async fn remove_favorite_is_idempotent_absent_is_success() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let favorites = FavoritesRepo::new(pool.clone());
    let account = seed_account(&pool, "fav@example.com").await;
    let net = seed_net(&defs, account, "tok-rm", Visibility::Listed).await;

    // Removing a favorite that was never added is a success no-op, not an error.
    favorites
        .remove(account, net)
        .await
        .expect("removing an absent favorite is a no-op success");
    assert_eq!(favorite_row_count(&pool, account).await, 0);

    favorites
        .add(account, net, now_millis())
        .await
        .expect("favorite");
    assert_eq!(favorite_row_count(&pool, account).await, 1);
    favorites.remove(account, net).await.expect("unfavorite");
    assert_eq!(
        favorite_row_count(&pool, account).await,
        0,
        "unfavorite deletes the row"
    );
    // A second remove is still a success no-op.
    favorites
        .remove(account, net)
        .await
        .expect("second remove is idempotent");
}

#[tokio::test]
async fn list_for_account_is_scoped_ordered_and_includes_unlisted_and_archived() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let favorites = FavoritesRepo::new(pool.clone());
    let now = now_millis();

    let alice = seed_account(&pool, "alice@example.com").await;
    let bob = seed_account(&pool, "bob@example.com").await;

    // Alice favorites three nets: a Listed one, an Unlisted one, and one that
    // is later archived. My Nets is a PRIVATE view — unlike public discovery it
    // must surface Unlisted AND archived favorites.
    let listed = seed_net(&defs, alice, "tok-listed", Visibility::Listed).await;
    let unlisted = seed_net(&defs, alice, "tok-unlisted", Visibility::Unlisted).await;
    let archived = seed_net(&defs, alice, "tok-archived", Visibility::Listed).await;
    defs.archive(archived, now).await.expect("archive the net");

    // Bob favorites his own net — it must never appear in Alice's list.
    let bobs_net = seed_net(&defs, bob, "tok-bob", Visibility::Listed).await;

    favorites.add(alice, listed, now).await.expect("fav listed");
    favorites
        .add(alice, unlisted, now)
        .await
        .expect("fav unlisted");
    favorites
        .add(alice, archived, now)
        .await
        .expect("fav archived");
    favorites
        .add(bob, bobs_net, now)
        .await
        .expect("bob favs his own");

    // Pin created_at to distinct instants so the created_at-DESC order is
    // deterministic (newest favorite first) rather than wall-clock-dependent.
    for (net, at) in [
        (listed, now),
        (unlisted, now + 1000),
        (archived, now + 2000),
    ] {
        sqlx::query(
            "UPDATE net_favorites SET created_at = to_timestamp($3::double precision / 1000)
             WHERE account_id = $1 AND net_definition_id = $2",
        )
        .bind(alice)
        .bind(net)
        .bind(at as i64)
        .execute(&pool)
        .await
        .expect("pin created_at");
    }

    let rows = favorites
        .list_for_account(alice)
        .await
        .expect("list Alice's favorites");

    // Account-scoped: exactly Alice's three, Bob's excluded.
    assert_eq!(rows.len(), 3, "only Alice's three favorites");
    let ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.id).collect();
    assert!(
        !ids.contains(&bobs_net),
        "Bob's favorite is not in Alice's list"
    );

    // Deterministic newest-first order by favorited_at (created_at DESC).
    assert_eq!(
        ids,
        vec![archived, unlisted, listed],
        "favorites are ordered newest-favorited first"
    );

    // The Unlisted favorite is present with its link token (return-link).
    let unlisted_row = rows
        .iter()
        .find(|r| r.id == unlisted)
        .expect("unlisted present");
    assert_eq!(unlisted_row.link_token, "tok-unlisted");
    assert!(
        unlisted_row.archived_at_millis.is_none(),
        "the unlisted net is not archived"
    );

    // The archived favorite is present WITH its archival instant (indicator),
    // never silently dropped.
    let archived_row = rows
        .iter()
        .find(|r| r.id == archived)
        .expect("archived present");
    assert_eq!(archived_row.archived_at_millis, Some(now));

    // Bob's own list contains only his net.
    let bob_rows = favorites.list_for_account(bob).await.expect("list Bob");
    assert_eq!(bob_rows.len(), 1);
    assert_eq!(bob_rows[0].id, bobs_net);
}

#[tokio::test]
async fn list_for_account_carries_display_fields_from_the_definition() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let favorites = FavoritesRepo::new(pool.clone());
    let account = seed_account(&pool, "fav@example.com").await;
    let net = seed_net(&defs, account, "tok-fields", Visibility::Listed).await;
    favorites
        .add(account, net, now_millis())
        .await
        .expect("favorite");

    let rows = favorites.list_for_account(account).await.expect("list");
    let row = rows.first().expect("one favorite");
    assert_eq!(row.id, net);
    assert_eq!(row.title, "Sunday Traffic Net");
    assert_eq!(
        row.connections.connections()[0].kind,
        NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band: Band::TwentyMeters,
            mode: Mode::Ssb,
        },
        "the favorites read carries the net's ways in"
    );
    assert_eq!(row.net_category, NetCategory::Traffic);
    assert_eq!(row.net_type, NetType::Open);
    assert_eq!(row.grid.as_deref(), Some("FN31pr"));
    assert_eq!(row.expected_duration_minutes, Some(90));
    assert_eq!(row.link_token, "tok-fields");
}

// --- Paged favorites / owned reads ------------------------------

#[tokio::test]
async fn favorites_page_walks_every_row_exactly_once_newest_first() {
    // The keyset walk is total. Five favorites, TWO of
    // which share a favorited_at millisecond — seeded through `add(…, now_millis)`
    // so the shared instant is a fixture fact — and built so that shared pair
    // straddles the limit=2 page edge, asserted directly rather than assumed. A
    // `created_at`-only cursor skips or repeats a row there; the row-value
    // `(created_at, net_definition_id)` cursor does not.
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let favorites = FavoritesRepo::new(pool.clone());
    let account = seed_account(&pool, "fav-walker@example.com").await;
    let owner = seed_account(&pool, "owner@example.com").await;

    let base = now_millis();
    let mut seeded: Vec<(u64, uuid::Uuid)> = Vec::new();
    for (i, offset) in [1_000_u64, 2_000, 3_000, 3_000, 4_000].iter().enumerate() {
        let net = seed_net(&defs, owner, &format!("tok-fw-{i}"), Visibility::Listed).await;
        favorites
            .add(account, net, base + offset)
            .await
            .expect("seed favorite");
        seeded.push((base + offset, net));
    }

    let first = favorites
        .list_for_account_page(account, 2, None)
        .await
        .expect("page 1");
    assert_eq!(first.rows.len(), 2);
    let resume = first.next.expect("a second page exists");
    let second = favorites
        .list_for_account_page(account, 2, Some(resume))
        .await
        .expect("page 2");
    assert_eq!(second.rows.len(), 2);
    // THE FIXTURE'S OWN PRECONDITION: the tie really does straddle this edge.
    assert_eq!(
        first.rows[1].favorited_at_millis, second.rows[0].favorited_at_millis,
        "page 1's last row and page 2's first row must share a favorited_at, \
         or this test proves nothing about a shared-timestamp boundary"
    );

    let mut walked: Vec<(u64, uuid::Uuid)> = Vec::new();
    let mut cursor = None;
    for _ in 0..10 {
        let page = favorites
            .list_for_account_page(account, 2, cursor)
            .await
            .expect("walk page");
        walked.extend(page.rows.iter().map(|r| (r.favorited_at_millis, r.id)));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(walked.len(), 5, "no duplicates and no gaps: {walked:?}");
    let mut expected = seeded.clone();
    expected.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        walked, expected,
        "exactly the seeded rows, strictly newest-first by (favorited_at, id)"
    );
}

#[tokio::test]
async fn owned_page_walks_every_row_exactly_once_newest_first() {
    // The same walk over the owned-nets read, which
    // orders by (created_at DESC, id DESC) — `create` takes the clock, so the
    // shared instant is pinned through the repo rather than by UPDATE. The
    // archived net and another account's net stay out, as the read it replaces
    // promised.
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner-walker@example.com").await;
    let other = seed_account(&pool, "other@example.com").await;

    let base = now_millis();
    let mut seeded: Vec<(u64, uuid::Uuid)> = Vec::new();
    for (i, offset) in [1_000_u64, 2_000, 3_000, 3_000, 4_000].iter().enumerate() {
        let net = repo
            .create(
                &sample_fields(),
                &sample_connections(),
                owner,
                &format!("tok-ow-{i}"),
                base + offset,
            )
            .await
            .expect("create owned");
        seeded.push((base + offset, net.id));
    }
    repo.create(
        &sample_fields(),
        &sample_connections(),
        other,
        "tok-ow-other",
        base + 5_000,
    )
    .await
    .expect("another account's net");
    let archived = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-ow-archived",
            base + 6_000,
        )
        .await
        .expect("net to archive");
    repo.archive(archived.id, base + 7_000)
        .await
        .expect("archive");

    let first = repo.list_owned_page(owner, 2, None).await.expect("page 1");
    assert_eq!(first.rows.len(), 2);
    let resume = first.next.expect("a second page exists");
    let second = repo
        .list_owned_page(owner, 2, Some(resume))
        .await
        .expect("page 2");
    assert_eq!(second.rows.len(), 2);
    assert_eq!(
        first.rows[1].created_at_millis, second.rows[0].created_at_millis,
        "page 1's last row and page 2's first row must share a created_at, \
         or this test proves nothing about a shared-timestamp boundary"
    );

    let mut walked: Vec<(u64, uuid::Uuid)> = Vec::new();
    let mut cursor = None;
    for _ in 0..10 {
        let page = repo
            .list_owned_page(owner, 2, cursor)
            .await
            .expect("walk page");
        assert!(
            page.rows.iter().all(|d| d.owner_account_ids == vec![owner]),
            "owners populated on every paged row"
        );
        walked.extend(page.rows.iter().map(|d| (d.created_at_millis, d.id)));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(walked.len(), 5, "no duplicates and no gaps: {walked:?}");
    let mut expected = seeded.clone();
    expected.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        walked, expected,
        "exactly the owner's active nets, strictly newest-first by (created_at, id)"
    );
}

#[tokio::test]
async fn two_favorites_written_in_one_millisecond_order_the_same_through_both_reads() {
    // `add` is clock-written at millisecond precision, so two favorites
    // sharing a `created_at` is routine (two rapid
    // PUTs, every fixed-clock test) and the tiebreak decides their order. The
    // personal-data export (`list_for_account`) and the My Nets page
    // (`list_for_account_page`) are two views of ONE collection and must not
    // disagree about which of the pair came first: both are `(created_at DESC,
    // net_definition_id DESC)`, pinned here through both reads, not one.
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let favorites = FavoritesRepo::new(pool.clone());
    let account = seed_account(&pool, "tied@example.com").await;
    let owner = seed_account(&pool, "owner@example.com").await;
    let shared_instant = now_millis();

    let first = seed_net(&defs, owner, "tok-tie-1", Visibility::Listed).await;
    let second = seed_net(&defs, owner, "tok-tie-2", Visibility::Listed).await;
    favorites
        .add(account, first, shared_instant)
        .await
        .expect("favorite one");
    favorites
        .add(account, second, shared_instant)
        .await
        .expect("favorite two");

    let unpaged: Vec<uuid::Uuid> = favorites
        .list_for_account(account)
        .await
        .expect("export read")
        .iter()
        .map(|r| r.id)
        .collect();
    let paged: Vec<uuid::Uuid> = favorites
        .list_for_account_page(account, 10, None)
        .await
        .expect("page read")
        .rows
        .iter()
        .map(|r| r.id)
        .collect();

    // THE FIXTURE'S OWN PRECONDITION: the pair really does tie on created_at.
    assert_eq!(unpaged.len(), 2, "both favorites: {unpaged:?}");
    let mut expected = vec![first, second];
    expected.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        paged, expected,
        "the page read breaks the tie on net_definition_id DESC"
    );
    assert_eq!(
        unpaged, paged,
        "the export and the page disagree about which same-millisecond favorite came first"
    );
}

// --- Net session event log + snapshot -------------------------

/// Creates a definition and a started session from it, returning the session
/// repo, the event log, and the new session id — the shared arrange step.
async fn seed_session(pool: &PgPool) -> (NetSessionRepo, SessionEventLog, uuid::Uuid) {
    let defs = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(pool, "session-owner@example.com").await;
    let definition = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-session",
            now_millis(),
        )
        .await
        .expect("create definition");
    let sessions = NetSessionRepo::new(pool.clone());
    let session = sessions
        .create(&definition, now_millis())
        .await
        .expect("create session");
    let log = SessionEventLog::new(pool.clone());
    (sessions, log, session.id)
}

#[tokio::test]
async fn concurrent_appends_to_one_session_assign_a_gapless_1_to_n_seq_set() {
    let (_container, pool) = migrated_pool().await;
    let (_sessions, log, session) = seed_session(&pool).await;

    // N truly-concurrent appends to ONE session. The row lock the in-txn
    // `last_seq` bump takes must serialize them so the assigned seq set is
    // exactly 1..=N — no gaps, no duplicates (the concurrency crux).
    const N: u64 = 50;
    let mut handles = Vec::new();
    for _ in 0..N {
        let log = log.clone();
        handles.push(tokio::spawn(async move {
            log.append(
                session,
                &SessionEventBody::FrequencyChanged {
                    connection_id: uuid::Uuid::from_u128(9),
                    operating_frequency_hz: 7_200_000,
                },
                None,
                now_millis(),
            )
            .await
        }));
    }

    let mut returned = Vec::new();
    for handle in handles {
        let event = handle
            .await
            .expect("append task joins")
            .expect("each concurrent append succeeds");
        returned.push(event.seq);
    }
    returned.sort_unstable();
    let expected: Vec<u64> = (1..=N).collect();
    assert_eq!(
        returned, expected,
        "the returned seq set is exactly 1..=N under concurrency"
    );

    let persisted: Vec<u64> = log
        .events_since(session, 0)
        .await
        .expect("read the whole log")
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(
        persisted, expected,
        "the persisted seqs are 1..=N ascending — gapless, duplicate-free, ordered"
    );
}

#[tokio::test]
async fn each_appended_body_round_trips_through_events_since() {
    let (_container, pool) = migrated_pool().await;
    let (_sessions, log, session) = seed_session(&pool).await;

    let actor = seed_account(&pool, "op-actor@example.com").await;
    let check_in_id = uuid::Uuid::now_v7();
    let client_event_id = uuid::Uuid::now_v7();
    let callsign = parse_callsign("W1AW").expect("valid callsign");

    // One of each of the four bodies: with/without actor, a checkin WITH and a
    // checkin WITHOUT client_event_id, a real Callsign.
    let inputs: Vec<(SessionEventBody, Option<uuid::Uuid>, u64)> = vec![
        (
            SessionEventBody::SessionStarted {
                definition_id: uuid::Uuid::now_v7(),
                definition_version: 3,
            },
            Some(actor),
            1_000,
        ),
        (
            SessionEventBody::FrequencyChanged {
                connection_id: uuid::Uuid::from_u128(9),
                operating_frequency_hz: 7_200_000,
            },
            None,
            2_000,
        ),
        (
            SessionEventBody::CheckinAdded {
                check_in_id,
                callsign: callsign.clone(),
                client_event_id: Some(client_event_id),
                signal_report: None,
                staying: StayingStatus::InAndOut,
                name: None,
                location: None,
                grid: None,
                source: CheckInSource::Staff,
                via: None,
                relayed_by: None,
            },
            Some(actor),
            3_000,
        ),
        (
            SessionEventBody::CheckinAdded {
                check_in_id: uuid::Uuid::now_v7(),
                callsign: callsign.clone(),
                client_event_id: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                name: None,
                location: None,
                grid: None,
                source: CheckInSource::Staff,
                via: None,
                relayed_by: None,
            },
            None,
            4_000,
        ),
        (SessionEventBody::SessionClosed, Some(actor), 5_000),
    ];

    let mut appended: Vec<SessionEvent> = Vec::new();
    for (body, actor_id, at) in &inputs {
        appended.push(
            log.append(session, body, *actor_id, *at)
                .await
                .expect("append"),
        );
    }

    let read = log.events_since(session, 0).await.expect("read whole log");
    assert_eq!(read.len(), 5);
    for (i, event) in read.iter().enumerate() {
        assert_eq!(event.seq, (i as u64) + 1, "seq is 1-indexed and monotonic");
        assert_eq!(
            event, &appended[i],
            "each reconstructed event equals what was appended (envelope + body)"
        );
    }

    // The `None` client_event_id is OMITTED from the stored JSON (not stored
    // as null), per the omit-optional-fields wire rule.
    let raw_no_cid: String = sqlx::query_scalar(
        "SELECT payload::text FROM session_events WHERE session_id = $1 AND seq = 4",
    )
    .bind(session)
    .fetch_one(&pool)
    .await
    .expect("read raw payload");
    assert!(
        !raw_no_cid.contains("clientEventId"),
        "clientEventId is omitted from JSON when None, got {raw_no_cid}"
    );

    // The keys that ARE present are genuinely camelCase on the wire (not just
    // "clientEventId happens to be absent") — checks the positive case the
    // omission assertion above cannot: seq=3 is a checkin WITH a
    // client_event_id, so every field name should surface here.
    let raw_with_cid: String = sqlx::query_scalar(
        "SELECT payload::text FROM session_events WHERE session_id = $1 AND seq = 3",
    )
    .bind(session)
    .fetch_one(&pool)
    .await
    .expect("read raw payload");
    for camel_key in ["checkInId", "callsign", "clientEventId"] {
        assert!(
            raw_with_cid.contains(camel_key),
            "expected camelCase key {camel_key} in stored payload, got {raw_with_cid}"
        );
    }
    for snake_key in ["check_in_id", "client_event_id"] {
        assert!(
            !raw_with_cid.contains(snake_key),
            "did not expect snake_case key {snake_key} in stored payload, got {raw_with_cid}"
        );
    }

    // events_since(k) returns strictly seq > k.
    let tail = log.events_since(session, 3).await.expect("tail read");
    assert_eq!(tail.len(), 2, "only seq 4 and 5 are past since=3");
    assert_eq!(tail[0].seq, 4);
    assert_eq!(tail[1].seq, 5);
}

#[tokio::test]
async fn appending_to_a_missing_session_is_row_not_found_not_silent_success() {
    let (_container, pool) = migrated_pool().await;
    let log = SessionEventLog::new(pool);
    let result = log
        .append(
            uuid::Uuid::now_v7(),
            &SessionEventBody::SessionClosed,
            None,
            now_millis(),
        )
        .await;
    assert!(
        matches!(result, Err(sqlx::Error::RowNotFound)),
        "an append to a nonexistent session must surface as RowNotFound, got {result:?}"
    );
}

#[tokio::test]
async fn a_definition_edit_after_session_create_never_mutates_the_session_snapshot() {
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let sessions = NetSessionRepo::new(pool.clone());
    let owner = seed_account(&pool, "snap-owner@example.com").await;

    let definition = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-snap",
            now_millis(),
        )
        .await
        .expect("create definition");
    assert_eq!(definition.definition_version, 1);

    let session = sessions
        .create(&definition, now_millis())
        .await
        .expect("create session");

    // Edit the SAME definition after the session started: change the title,
    // which bumps definition_version server-side.
    let mut edited = sample_fields();
    edited.title = "Renamed Net".to_owned();
    let updated = defs
        .update(definition.id, &edited, now_millis())
        .await
        .expect("update definition");
    assert_eq!(
        updated.definition_version, 2,
        "the edit bumped the definition version"
    );
    assert_eq!(updated.title, "Renamed Net");

    // The session's frozen snapshot ignores the later edit entirely.
    let found = sessions
        .find(session.id)
        .await
        .expect("find session")
        .expect("session exists");
    assert_eq!(
        found.definition_version, 1,
        "provenance is the version at start, not the edited one"
    );
    assert_eq!(
        found.definition_snapshot.title, "Sunday Traffic Net",
        "the snapshot title is frozen at create, unchanged by the edit"
    );
    assert_eq!(found.definition_id, definition.id);
    assert_eq!(found.lifecycle, SessionLifecycle::Live);
    // The session copies the definition's connection SET by value,
    // ids included, and there is no session-level frequency to compare.
    assert_eq!(
        found
            .definition_snapshot
            .connections
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        definition
            .connections
            .connections()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        "the snapshot carries the DEFINITION's connection ids, not fresh ones"
    );
    assert_eq!(
        found.last_seq, 0,
        "a freshly-created session has no events yet"
    );
}

#[tokio::test]
async fn find_returns_none_for_an_unknown_session() {
    let (_container, pool) = migrated_pool().await;
    let sessions = NetSessionRepo::new(pool);
    let missing = sessions
        .find(uuid::Uuid::now_v7())
        .await
        .expect("find query");
    assert!(missing.is_none(), "an unknown session id reads as None");
}

// --- Atomic start/close composition + close projection ---------

/// Creates a definition and returns the session repo, the event log, and the
/// definition — the arrange step for the atomic start/close tests.
async fn seed_definition(
    pool: &PgPool,
    email: &str,
) -> (
    NetSessionRepo,
    SessionEventLog,
    netroll_domain::net::NetDefinition,
) {
    let defs = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(pool, email).await;
    let definition = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-start-close",
            now_millis(),
        )
        .await
        .expect("create definition");
    (
        NetSessionRepo::new(pool.clone()),
        SessionEventLog::new(pool.clone()),
        definition,
    )
}

/// Counts `net_sessions` rows for one definition — the arrange/assert helper
/// for the one-live-per-definition tests.
async fn session_count_for(pool: &PgPool, definition_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_sessions WHERE definition_id = $1")
        .bind(definition_id)
        .fetch_one(pool)
        .await
        .expect("count sessions for definition")
}

#[tokio::test]
async fn atomic_start_composes_the_row_and_the_first_event_at_seq_1() {
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "start-owner@example.com").await;
    let actor = seed_account(&pool, "op-actor@example.com").await;

    let row = match sessions
        .start(&definition, Some(actor), now_millis())
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    assert_eq!(row.lifecycle, SessionLifecycle::Live);
    assert_eq!(row.definition_id, definition.id);

    // The single first event is the session.started at seq=1 — the row-locked
    // seq bump reused verbatim (last_seq 0 -> 1), no new counter logic.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 1, "start writes exactly the first event");
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[0].actor_id, Some(actor));
    assert!(matches!(
        events[0].body,
        SessionEventBody::SessionStarted { .. }
    ));

    // Folding the log reproduces a Live state — the atomicity invariant: a
    // started session never has a row without its session.started event.
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.lifecycle, SessionLifecycle::Live);
    assert_eq!(state.last_seq, 1);
    // Starting moves no frequency — each connection is on the one
    // the frozen snapshot recorded for it.
    assert!(state.connection_frequencies.is_empty());

    // The persisted row's counter advanced to 1 in the same transaction.
    let found = sessions
        .find(row.id)
        .await
        .expect("find")
        .expect("session exists");
    assert_eq!(found.last_seq, 1);
    assert_eq!(found.lifecycle, SessionLifecycle::Live);
    assert!(found.closed_at_millis.is_none());
}

#[tokio::test]
async fn find_live_session_id_by_definition_tracks_the_live_window() {
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) =
        seed_definition(&pool, "live-lookup-owner@example.com").await;

    assert_eq!(
        sessions
            .find_live_session_id_by_definition(definition.id)
            .await
            .expect("query"),
        None,
        "no session started yet"
    );

    let actor = seed_account(&pool, "live-lookup-actor@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    assert_eq!(
        sessions
            .find_live_session_id_by_definition(definition.id)
            .await
            .expect("query"),
        Some(row.id),
        "the My Nets Owned tab's live badge reads this"
    );

    sessions
        .close(row.id, started_at + 1, Some(actor))
        .await
        .expect("close");

    assert_eq!(
        sessions
            .find_live_session_id_by_definition(definition.id)
            .await
            .expect("query"),
        None,
        "a closed session is no longer live"
    );
}

#[tokio::test]
async fn atomic_close_appends_session_closed_and_flips_the_projection() {
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "close-owner@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, None, started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let closed_at = started_at + 5_000;
    let outcome = sessions
        .close(row.id, closed_at, None)
        .await
        .expect("atomic close");
    assert!(
        matches!(outcome, CloseOutcome::Closed(_)),
        "expected Closed, got {outcome:?}"
    );

    // The session.closed event is appended at the next seq (=2).
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].seq, 2);
    assert!(matches!(events[1].body, SessionEventBody::SessionClosed));

    // The projection columns flipped in the SAME transaction as the append.
    let found = sessions
        .find(row.id)
        .await
        .expect("find")
        .expect("session exists");
    assert_eq!(found.lifecycle, SessionLifecycle::Closed);
    assert_eq!(found.closed_at_millis, Some(closed_at));
    assert_eq!(found.last_seq, 2);

    // The folded log and the projection column agree — they can never diverge.
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.lifecycle, SessionLifecycle::Closed);
    assert_eq!(state.closed_at, Some(closed_at));
}

#[tokio::test]
async fn change_frequency_appends_frequency_changed_naming_the_connection_it_moves() {
    // The guarded live-gate and the `frequency.changed` append commit in ONE
    // transaction, and the
    // event NAMES the snapshot connection it moves.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "freq-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let connection_id = row.definition_snapshot.connections[0].id;
    let changed_at = started_at + 5_000;
    let outcome = sessions
        .change_frequency(row.id, connection_id, 7_200_000, Some(actor), changed_at)
        .await
        .expect("change frequency");
    let event = match outcome {
        ChangeFrequencyOutcome::Changed(event) => event,
        other => panic!("expected Changed, got {other:?}"),
    };
    // The returned event is the exact appended frequency.changed at seq 2.
    assert_eq!(event.seq, 2);
    assert_eq!(event.actor_id, Some(actor));
    assert!(matches!(
        event.body,
        SessionEventBody::FrequencyChanged {
            operating_frequency_hz: 7_200_000,
            ..
        }
    ));

    // Exactly one frequency.changed event appended at the next seq (=2).
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].seq, 2);
    assert!(matches!(
        events[1].body,
        SessionEventBody::FrequencyChanged {
            operating_frequency_hz: 7_200_000,
            ..
        }
    ));

    // The session stays live (a frequency change is not a lifecycle
    // transition) and its FROZEN snapshot is not rewritten — the move
    // lives in the log, and the fold overlays it.
    let found = sessions
        .find(row.id)
        .await
        .expect("find")
        .expect("session exists");
    assert_eq!(found.lifecycle, SessionLifecycle::Live);
    assert_eq!(found.last_seq, 2);
    assert_eq!(
        found.definition_snapshot.connections[0].planned_frequency_hz,
        Some(14_230_000),
        "a running session's stored snapshot is never rewritten"
    );

    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(
        state.connection_frequencies.get(&connection_id),
        Some(&7_200_000)
    );
    assert_eq!(state.lifecycle, SessionLifecycle::Live);
}

#[tokio::test]
async fn changing_the_frequency_of_a_connection_this_session_never_froze_is_refused() {
    // The event names a connection, so a name the session's own
    // snapshot does not carry must be refused — a frequency written against it
    // would be invisible on every surface, because every one renders that set.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "freq-unknown@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, None, started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let outcome = sessions
        .change_frequency(
            row.id,
            uuid::Uuid::from_u128(0xDEAD),
            7_200_000,
            None,
            started_at + 1_000,
        )
        .await
        .expect("change_frequency returns an outcome, not a db error");
    assert!(
        matches!(outcome, ChangeFrequencyOutcome::ConnectionUnknown),
        "got {outcome:?}"
    );
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 1, "no phantom frequency.changed was appended");
}

#[tokio::test]
async fn changing_frequency_on_a_closed_session_is_not_live_and_appends_nothing() {
    // The atomic `AND lifecycle = 'live'` guard is the
    // real authority — a closed session refuses the change with no phantom
    // event, exactly like `close`'s guarded write.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "freq-closed@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, None, started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let closed = match sessions
        .close(row.id, started_at + 1_000, None)
        .await
        .expect("close")
    {
        CloseOutcome::Closed(event) => event,
        other => panic!("expected Closed, got {other:?}"),
    };
    assert_eq!(closed.seq, 2);

    let connection_id = row.definition_snapshot.connections[0].id;
    let outcome = sessions
        .change_frequency(row.id, connection_id, 7_200_000, None, started_at + 2_000)
        .await
        .expect("change_frequency returns an outcome, not a db error, on a closed session");
    assert!(
        matches!(outcome, ChangeFrequencyOutcome::NotLive),
        "a closed session refuses the change as NotLive, got {outcome:?}"
    );

    // No frequency.changed event was appended, and the frequency is unchanged.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    let freq_events = events
        .iter()
        .filter(|e| matches!(e.body, SessionEventBody::FrequencyChanged { .. }))
        .count();
    assert_eq!(
        freq_events, 0,
        "no phantom frequency.changed on a closed session"
    );
    let found = sessions
        .find(row.id)
        .await
        .expect("find")
        .expect("session exists");
    assert_eq!(
        found.lifecycle,
        SessionLifecycle::Closed,
        "frequency frozen"
    );
}

#[tokio::test]
async fn changing_frequency_on_a_missing_session_is_the_missing_outcome() {
    let (_container, pool) = migrated_pool().await;
    let sessions = NetSessionRepo::new(pool);
    let outcome = sessions
        .change_frequency(
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            7_200_000,
            None,
            now_millis(),
        )
        .await
        .expect("change_frequency returns an outcome, not a db error, for a missing session");
    assert!(
        matches!(outcome, ChangeFrequencyOutcome::Missing),
        "changing frequency on a nonexistent session surfaces Missing, got {outcome:?}"
    );
}

#[tokio::test]
async fn add_check_in_appends_checkin_added_and_folds_a_one_row_roster() {
    // The guarded live-gate write and the checkin.added
    // append commit in ONE transaction; the folded log carries the new roster
    // row and last_seq advances — the atomic add mirrors change_frequency.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "checkin-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let check_in_id = uuid::Uuid::now_v7();
    let callsign = parse_callsign("W1AW").expect("valid callsign");
    let outcome = sessions
        .add_check_in(
            row.id,
            &callsign,
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add check-in");
    let event = match outcome {
        AddCheckInOutcome::Added(applied) => applied.added,
        other => panic!("expected Added, got {other:?}"),
    };
    // The returned event is the exact appended checkin.added at seq 2.
    assert_eq!(event.seq, 2);
    assert_eq!(event.actor_id, Some(actor));
    assert!(matches!(
        &event.body,
        SessionEventBody::CheckinAdded { check_in_id: id, callsign: c, client_event_id: None, .. }
            if *id == check_in_id && c.as_str() == "W1AW"
    ));

    // Exactly one checkin.added appended at seq 2, folding to a one-row roster.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 2);
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster.len(), 1);
    assert_eq!(state.roster[0].callsign.as_str(), "W1AW");
    assert_eq!(state.roster[0].check_in_id, check_in_id);

    // The session stays live (a check-in is not a lifecycle transition).
    let found = sessions
        .find(row.id)
        .await
        .expect("find")
        .expect("session exists");
    assert_eq!(found.lifecycle, SessionLifecycle::Live);
    assert_eq!(found.last_seq, 2);
}

#[tokio::test]
async fn roster_memory_upsert_is_last_write_wins_recency_and_coalesce_keeps_last_non_null() {
    // The projection upsert keys on (definition_id,
    // callsign); a later callsign-only add never erases a remembered name
    // (COALESCE-keep-last-non-null), last_seen_at is last-write-wins recency,
    // and check_in_count bumps on each upsert.
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _log, definition) = seed_definition(&pool, "roster-mem@example.com").await;
    let repo = RosterMemoryRepo::new(pool.clone());

    // A miss is None.
    assert_eq!(
        repo.lookup(definition.id, "W1AW").await.expect("lookup"),
        None
    );

    // First upsert: full identity recorded.
    repo.upsert(
        definition.id,
        "W1AW",
        Some("Maria"),
        Some("Hartford, CT"),
        1_000,
    )
    .await
    .expect("insert");
    let first = repo
        .lookup(definition.id, "W1AW")
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(first.name.as_deref(), Some("Maria"));
    assert_eq!(first.location.as_deref(), Some("Hartford, CT"));

    // A later callsign-only add (name/location None) must NOT erase the
    // remembered name/location (COALESCE-keep-last-non-null).
    repo.upsert(definition.id, "W1AW", None, None, 2_000)
        .await
        .expect("callsign-only update");
    let kept = repo
        .lookup(definition.id, "W1AW")
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(kept.name.as_deref(), Some("Maria"), "name is NOT erased");
    assert_eq!(
        kept.location.as_deref(),
        Some("Hartford, CT"),
        "location is NOT erased"
    );

    // A later add carrying a NEW name overwrites (last-write-wins on the
    // supplied value); an absent location is still kept.
    repo.upsert(definition.id, "W1AW", Some("Maria K."), None, 3_000)
        .await
        .expect("named update");
    let updated = repo
        .lookup(definition.id, "W1AW")
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(updated.name.as_deref(), Some("Maria K."));
    assert_eq!(updated.location.as_deref(), Some("Hartford, CT"));

    // check_in_count bumped once per upsert (3 upserts → 3), and last_seen_at is
    // the most recent event's instant.
    let (count, last_seen): (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "SELECT check_in_count, last_seen_at FROM net_definition_roster
         WHERE definition_id = $1 AND callsign = $2",
    )
    .bind(definition.id)
    .bind("W1AW")
    .fetch_one(&pool)
    .await
    .expect("read metadata");
    assert_eq!(count, 3, "check_in_count bumps once per upsert");
    assert_eq!(
        last_seen.timestamp_millis(),
        3_000,
        "recency is last-write-wins"
    );
}

#[tokio::test]
async fn roster_memory_is_scoped_to_exactly_one_definition() {
    // A callsign remembered on definition A never appears
    // for definition B — the (definition_id, callsign) key is the scope, a
    // correctness and privacy boundary.
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "two-defs@example.com").await;
    let def_a = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-def-a",
            now_millis(),
        )
        .await
        .expect("definition A");
    let def_b = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-def-b",
            now_millis(),
        )
        .await
        .expect("definition B");
    let repo = RosterMemoryRepo::new(pool.clone());

    repo.upsert(def_a.id, "W1AW", Some("Maria"), Some("Hartford, CT"), 1_000)
        .await
        .expect("remember on A");

    // A holds the memory; B does not (a different remembered value would leak
    // an operator-entered identity across nets).
    assert_eq!(
        repo.lookup(def_a.id, "W1AW")
            .await
            .expect("lookup A")
            .expect("present")
            .name
            .as_deref(),
        Some("Maria")
    );
    assert_eq!(
        repo.lookup(def_b.id, "W1AW").await.expect("lookup B"),
        None,
        "the same callsign on a DIFFERENT definition is a separate (absent) row"
    );

    // The same callsign on B is an INDEPENDENT row, not a shared one.
    repo.upsert(def_b.id, "W1AW", Some("Bob"), None, 2_000)
        .await
        .expect("remember on B");
    assert_eq!(
        repo.lookup(def_a.id, "W1AW")
            .await
            .expect("lookup A again")
            .expect("present")
            .name
            .as_deref(),
        Some("Maria"),
        "definition A's memory is untouched by a write to definition B"
    );
}

#[tokio::test]
async fn add_check_in_writes_roster_memory_in_the_same_commit() {
    // A committed checkin.added carrying name/location
    // upserts the per-net roster-memory row atomically with the append — the
    // next session (same definition) can then prefill the returning station.
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) = seed_definition(&pool, "mem-add@example.com").await;
    let memory = RosterMemoryRepo::new(pool.clone());
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let callsign = parse_callsign("W1AW").expect("valid");
    let name = parse_name("Maria").expect("valid");
    let location = parse_location("Hartford, CT").expect("valid");
    sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            name.as_ref(),
            location.as_ref(),
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add with identity");

    let remembered = memory
        .lookup(definition.id, "W1AW")
        .await
        .expect("lookup")
        .expect("memory written in the same commit");
    assert_eq!(remembered.name.as_deref(), Some("Maria"));
    assert_eq!(remembered.location.as_deref(), Some("Hartford, CT"));
}

#[tokio::test]
async fn a_check_in_on_a_closed_session_writes_no_roster_memory() {
    // A rolled-back add (here: NotLive on a closed session)
    // never writes memory — the upsert lives in the same transaction that the
    // guarded gate refuses, so nothing persists.
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) = seed_definition(&pool, "mem-closed@example.com").await;
    let memory = RosterMemoryRepo::new(pool.clone());
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    sessions
        .close(row.id, started_at + 500, Some(actor))
        .await
        .expect("close");

    let callsign = parse_callsign("W1AW").expect("valid");
    let name = parse_name("Maria").expect("valid");
    let outcome = sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            name.as_ref(),
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add returns an outcome");
    assert!(matches!(outcome, AddCheckInOutcome::NotLive));
    assert_eq!(
        memory.lookup(definition.id, "W1AW").await.expect("lookup"),
        None,
        "a refused check-in leaves no roster memory"
    );
}

#[tokio::test]
async fn a_duplicate_add_does_not_double_write_roster_memory() {
    // The second add with the SAME clientEventId rolls back
    // (idempotency index), so its memory upsert rolls back too — the count is
    // NOT bumped twice and the first add's identity stands.
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) = seed_definition(&pool, "mem-dup@example.com").await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let callsign = parse_callsign("W1AW").expect("valid");
    let name = parse_name("Maria").expect("valid");
    let client_event_id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            Some(client_event_id),
            None,
            StayingStatus::InAndOut,
            name.as_ref(),
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("first add");
    // The duplicate (same clientEventId) is refused with no phantom event.
    let dup = sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            Some(client_event_id),
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("duplicate add");
    assert!(matches!(dup, AddCheckInOutcome::Duplicate));

    let count: i64 = sqlx::query_scalar(
        "SELECT check_in_count FROM net_definition_roster
         WHERE definition_id = $1 AND callsign = $2",
    )
    .bind(definition.id)
    .bind("W1AW")
    .fetch_one(&pool)
    .await
    .expect("read count");
    assert_eq!(
        count, 1,
        "the rolled-back duplicate does not bump the memory count"
    );
}

#[tokio::test]
async fn a_callsign_only_add_after_a_named_add_keeps_the_remembered_name() {
    // A returning station logged callsign-only (a second
    // add of the same callsign, no name) must not erase the name the first add
    // remembered — COALESCE-keep-last-non-null through the real check-in path.
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) = seed_definition(&pool, "mem-coalesce@example.com").await;
    let memory = RosterMemoryRepo::new(pool.clone());
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let callsign = parse_callsign("W1AW").expect("valid");
    let name = parse_name("Maria").expect("valid");
    sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            name.as_ref(),
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("named add");
    // Same callsign, callsign-only (no name) — a distinct check-in row.
    sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("callsign-only add");

    let remembered = memory
        .lookup(definition.id, "W1AW")
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(
        remembered.name.as_deref(),
        Some("Maria"),
        "the callsign-only add did not erase the remembered name"
    );
}

#[tokio::test]
async fn edit_check_in_updates_roster_memory_but_a_remove_does_not() {
    // A checkin.updated writes the committed identity to
    // memory (so the next session prefills the corrected name); a checkin.removed
    // is a tombstone and never writes memory.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "mem-edit@example.com").await;
    let memory = RosterMemoryRepo::new(pool.clone());
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    let callsign = parse_callsign("W1AW").expect("valid");
    // Callsign-only add — no memory identity yet.
    sessions
        .add_check_in(
            row.id,
            &callsign,
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add");
    assert_eq!(
        memory
            .lookup(definition.id, "W1AW")
            .await
            .expect("lookup")
            .expect("row exists from the add")
            .name,
        None
    );

    // Edit sets the name → memory now remembers it (version 1 at add).
    let name = parse_name("Maria").expect("valid");
    let location = parse_location("Hartford, CT").expect("valid");
    let outcome = sessions
        .edit_check_in(
            row.id,
            check_in_id,
            1,
            &callsign,
            name.as_ref(),
            location.as_ref(),
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Routine,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("edit");
    assert!(matches!(outcome, EditCheckInOutcome::Applied(_)));
    let after_edit = memory
        .lookup(definition.id, "W1AW")
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(after_edit.name.as_deref(), Some("Maria"));
    assert_eq!(after_edit.location.as_deref(), Some("Hartford, CT"));

    // Remove the entry (version is now 2 after the edit) — memory is NOT touched.
    let removed = sessions
        .remove_check_in(row.id, check_in_id, 2, Some(actor), started_at + 3_000)
        .await
        .expect("remove");
    assert!(matches!(removed, EditCheckInOutcome::Applied(_)));
    let after_remove = memory
        .lookup(definition.id, "W1AW")
        .await
        .expect("lookup")
        .expect("memory survives the tombstone");
    assert_eq!(
        after_remove.name.as_deref(),
        Some("Maria"),
        "a checkin.removed does not rewrite roster memory"
    );
    // Sanity: the roster row itself is gone from the fold.
    let events = log.events_since(row.id, 0).await.expect("log");
    assert!(netroll_domain::fold::replay(&events, 0).roster.is_empty());
}

#[tokio::test]
async fn editing_a_check_in_to_deliberately_clear_the_name_clears_the_remembered_memory() {
    // `checkin.updated` carries the FULL
    // post-edit field set with REPLACE (not merge) semantics on the roster
    // entry itself (fold.rs) — if staff deliberately blank the Name field to
    // correct a mistaken entry, the roster-memory projection must reflect that
    // cleared value too. The COALESCE-keep-last-non-null policy is correct for
    // the ADD path (a later callsign-only add must not erase memory) but must
    // NOT apply to an explicit edit, or a wrong remembered name could never be
    // un-set — a real bug distinct from the ADD-path behavior this same file
    // already covers in `a_callsign_only_add_after_a_named_add_keeps_the_remembered_name`.
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) = seed_definition(&pool, "mem-clear@example.com").await;
    let memory = RosterMemoryRepo::new(pool.clone());
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    let callsign = parse_callsign("W1AW").expect("valid");
    let name = parse_name("Maria").expect("valid");
    let location = parse_location("Hartford, CT").expect("valid");
    sessions
        .add_check_in(
            row.id,
            &callsign,
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            name.as_ref(),
            location.as_ref(),
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("named add");
    assert_eq!(
        memory
            .lookup(definition.id, "W1AW")
            .await
            .expect("lookup")
            .expect("row exists from the add")
            .name
            .as_deref(),
        Some("Maria")
    );

    // Edit DELIBERATELY clears the name (and location) — the operator decided
    // "Maria" was wrong and wants no name recorded.
    let outcome = sessions
        .edit_check_in(
            row.id,
            check_in_id,
            1,
            &callsign,
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Routine,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("edit");
    assert!(matches!(outcome, EditCheckInOutcome::Applied(_)));

    let after_clear = memory
        .lookup(definition.id, "W1AW")
        .await
        .expect("lookup")
        .expect("row still exists");
    assert_eq!(
        after_clear.name, None,
        "a deliberate edit-clear must REPLACE the remembered name to None, not \
         COALESCE-preserve the stale value forever"
    );
    assert_eq!(
        after_clear.location, None,
        "a deliberate edit-clear must REPLACE the remembered location to None too"
    );
}

#[tokio::test]
async fn edit_check_in_appends_checkin_updated_behind_the_version_cas() {
    // An edit with the CORRECT expectedVersion appends
    // checkin.updated and bumps the folded version; a stale expectedVersion is
    // refused (StaleVersion) with no phantom event.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "edit-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    let callsign = parse_callsign("W1AW").expect("valid callsign");
    sessions
        .add_check_in(
            row.id,
            &callsign,
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add");

    // A stale version (0) is refused before any append.
    let stale = sessions
        .edit_check_in(
            row.id,
            check_in_id,
            0,
            &parse_callsign("W1AX").expect("valid"),
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Routine,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("edit call");
    assert!(matches!(stale, EditCheckInOutcome::StaleVersion));

    // The correct version (1) succeeds and appends checkin.updated at seq 3.
    let name = parse_name("Maria").expect("valid").expect("some");
    let location = parse_location("Hartford, CT")
        .expect("valid")
        .expect("some");
    let outcome = sessions
        .edit_check_in(
            row.id,
            check_in_id,
            1,
            &parse_callsign("W1AX").expect("valid"),
            Some(&name),
            Some(&location),
            None,
            None,
            StayingStatus::StayingForComments,
            Precedence::Routine,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            started_at + 3_000,
        )
        .await
        .expect("edit call");
    let event = match outcome {
        EditCheckInOutcome::Applied(event) => event,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(event.seq, 3);
    assert_eq!(event.body.kind(), "checkin.updated");

    // The log folds to a version-2 entry with the replaced fields.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster.len(), 1);
    assert_eq!(state.roster[0].callsign.as_str(), "W1AX");
    assert_eq!(state.roster[0].version, 2);
    assert_eq!(
        state.roster[0].name.as_ref().map(|n| n.as_str()),
        Some("Maria")
    );

    // A second edit at the STALE version 1 is now refused (the CAS moved to 2).
    let refused = sessions
        .edit_check_in(
            row.id,
            check_in_id,
            1,
            &parse_callsign("W1AW").expect("valid"),
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Routine,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            started_at + 4_000,
        )
        .await
        .expect("edit call");
    assert!(matches!(refused, EditCheckInOutcome::StaleVersion));
    // No phantom append — still exactly 3 events.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 3);
}

#[tokio::test]
async fn editing_round_trips_both_notes_through_the_log_independently() {
    // The two per-station notes persist and reload
    // as SEPARATE fields through the same append-only log — setting one never
    // moves the other, and neither needs a column, a migration or a backfill.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "notes-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    let callsign = parse_callsign("W1AW").expect("valid callsign");
    sessions
        .add_check_in(
            row.id,
            &callsign,
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add");

    let staff = netroll_domain::check_in::parse_note("STAFF: sounded rough")
        .expect("valid")
        .expect("some");
    let public = netroll_domain::check_in::parse_note("PUBLIC: relaying for W1BBB")
        .expect("valid")
        .expect("some");
    sessions
        .edit_check_in(
            row.id,
            check_in_id,
            1,
            &callsign,
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Routine,
            None,
            Some(&staff),
            Some(&public),
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("edit call");

    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(
        state.roster[0].notes.as_ref().map(|n| n.as_str()),
        Some("STAFF: sounded rough")
    );
    assert_eq!(
        state.roster[0].public_note.as_ref().map(|n| n.as_str()),
        Some("PUBLIC: relaying for W1BBB")
    );

    // Clearing ONLY the public note leaves the staff note where it was.
    sessions
        .edit_check_in(
            row.id,
            check_in_id,
            2,
            &callsign,
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Routine,
            None,
            Some(&staff),
            None,
            None,
            None,
            Some(actor),
            started_at + 3_000,
        )
        .await
        .expect("edit call");
    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(
        state.roster[0].notes.as_ref().map(|n| n.as_str()),
        Some("STAFF: sounded rough")
    );
    assert!(state.roster[0].public_note.is_none());
}

#[tokio::test]
async fn editing_round_trips_precedence_and_traffic_through_the_fold() {
    // An edit setting precedence=emergency + traffic=3 folds
    // onto the roster and derives the precedence/traffic corrections.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "prec-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add");

    let traffic = parse_traffic_count(Some(3)).expect("valid");
    let outcome = sessions
        .edit_check_in(
            row.id,
            check_in_id,
            1,
            &parse_callsign("W1AW").expect("valid"),
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Emergency,
            traffic,
            None,
            None,
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("edit call");
    assert!(matches!(outcome, EditCheckInOutcome::Applied(_)));

    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster[0].precedence, Precedence::Emergency);
    assert_eq!(state.roster[0].traffic.map(|t| t.get()), Some(3));
    assert!(
        state.roster[0]
            .corrections
            .iter()
            .any(|c| c.field == netroll_domain::fold::CorrectionField::Precedence)
    );
}

#[tokio::test]
async fn reorder_roster_appends_roster_reordered_and_orders_by_precedence() {
    // Reorder_roster folds the log, computes the stable
    // Emergency->Priority->Routine order, and appends roster.reordered so the
    // folded roster is re-sequenced.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "reorder-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    // Add three routine check-ins in order A, B, C.
    let mut ids = Vec::new();
    for (i, call) in ["W1AW", "N1CCK", "K1XYZ"].iter().enumerate() {
        let id = uuid::Uuid::now_v7();
        ids.push(id);
        sessions
            .add_check_in(
                row.id,
                &parse_callsign(call).expect("valid"),
                id,
                None,
                None,
                StayingStatus::InAndOut,
                None,
                None,
                None,
                CheckInSource::Staff,
                None,
                None,
                Some(actor),
                started_at + 1_000 + i as u64,
            )
            .await
            .expect("add");
    }
    // Promote the LAST one (C) to Emergency.
    sessions
        .edit_check_in(
            row.id,
            ids[2],
            1,
            &parse_callsign("K1XYZ").expect("valid"),
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Emergency,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("edit");

    let outcome = sessions
        .reorder_roster(row.id, Some(actor), started_at + 3_000)
        .await
        .expect("reorder call");
    let event = match outcome {
        ReorderOutcome::Reordered(event) => event,
        other => panic!("expected Reordered, got {other:?}"),
    };
    assert_eq!(event.body.kind(), "roster.reordered");

    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    let ordered: Vec<_> = state.roster.iter().map(|e| e.check_in_id).collect();
    // Emergency C first, then routine A, B (insertion order preserved).
    assert_eq!(ordered, vec![ids[2], ids[0], ids[1]]);
}

#[tokio::test]
async fn reorder_roster_is_a_noop_when_already_in_precedence_order() {
    // An already-sorted roster short-circuits with NoOp and no
    // roster.reordered event is appended (avoid log churn).
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "reorder-noop@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add");
    let before = log.events_since(row.id, 0).await.expect("log").len();

    let outcome = sessions
        .reorder_roster(row.id, Some(actor), started_at + 2_000)
        .await
        .expect("reorder call");
    assert!(matches!(outcome, ReorderOutcome::NoOp));
    let after = log.events_since(row.id, 0).await.expect("log").len();
    assert_eq!(before, after, "no roster.reordered appended for a no-op");
}

#[tokio::test]
async fn set_worked_station_appends_and_folds_the_cursor() {
    // Set_worked_station folds the log, validates the target,
    // and appends station.worked-set so the cursor folds onto the state.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "worked-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add");

    let outcome = sessions
        .set_worked_station(row.id, Some(id), Some(actor), started_at + 2_000)
        .await
        .expect("worked-station call");
    let event = match outcome {
        WorkedStationOutcome::Set(applied) => applied.set,
        other => panic!("expected Set, got {other:?}"),
    };
    assert_eq!(event.body.kind(), "station.worked-set");

    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.working_check_in_id, Some(id));

    // An off-roster target is refused with UnknownCheckIn, no append.
    let before = log.events_since(row.id, 0).await.expect("log").len();
    let refused = sessions
        .set_worked_station(
            row.id,
            Some(uuid::Uuid::now_v7()),
            Some(actor),
            started_at + 3_000,
        )
        .await
        .expect("worked-station call");
    assert!(matches!(refused, WorkedStationOutcome::UnknownCheckIn));
    let after = log.events_since(row.id, 0).await.expect("log").len();
    assert_eq!(before, after, "no append for an unknown target");

    // Clearing the cursor (None) appends and folds to None.
    let cleared = sessions
        .set_worked_station(row.id, None, Some(actor), started_at + 4_000)
        .await
        .expect("worked-station call");
    assert!(matches!(cleared, WorkedStationOutcome::Set(_)));
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(
        netroll_domain::fold::replay(&events, 0).working_check_in_id,
        None
    );
}

// --- The persistent worked-sink ordering mode --------------------

/// Seeds a live session with one staff check-in per entry of `calls`, returning
/// the repo, the log, the session id, the acting account and the check-in ids in
/// insertion order.
async fn seed_live_roster(
    pool: &PgPool,
    email: &str,
    calls: &[&str],
) -> (
    NetSessionRepo,
    SessionEventLog,
    uuid::Uuid,
    uuid::Uuid,
    Vec<uuid::Uuid>,
) {
    let (sessions, log, definition) = seed_definition(pool, email).await;
    let started_at = now_millis();
    let actor = seed_account(pool, &format!("actor-{email}")).await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let mut ids = Vec::new();
    for (i, call) in calls.iter().enumerate() {
        let id = uuid::Uuid::now_v7();
        ids.push(id);
        sessions
            .add_check_in(
                row.id,
                &parse_callsign(call).expect("valid"),
                id,
                None,
                None,
                StayingStatus::InAndOut,
                None,
                None,
                None,
                CheckInSource::Staff,
                None,
                None,
                Some(actor),
                started_at + 1_000 + i as u64,
            )
            .await
            .expect("add");
    }
    (sessions, log, row.id, actor, ids)
}

async fn folded_order(log: &SessionEventLog, session_id: uuid::Uuid) -> Vec<uuid::Uuid> {
    let events = log.events_since(session_id, 0).await.expect("read the log");
    netroll_domain::fold::replay(&events, 0)
        .roster
        .iter()
        .map(|entry| entry.check_in_id)
        .collect()
}

async fn event_kinds(log: &SessionEventLog, session_id: uuid::Uuid) -> Vec<&'static str> {
    log.events_since(session_id, 0)
        .await
        .expect("read the log")
        .iter()
        .map(|event| event.body.kind())
        .collect()
}

#[tokio::test]
async fn set_worked_station_under_worked_sink_appends_the_permutation_at_the_next_seq() {
    // With the mode ON, marking a station worked
    // appends station.worked-set AND the follow-on roster.reordered in ONE
    // transaction, at consecutive seqs, worked-set first.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, session_id, actor, ids) =
        seed_live_roster(&pool, "sink-a@example.com", &["W1AAA", "W1BBB", "W1CCC"]).await;
    let at = now_millis();
    sessions
        .set_roster_order_mode(
            session_id,
            netroll_domain::fold::RosterOrderMode::WorkedSink,
            Some(actor),
            at,
        )
        .await
        .expect("mode");
    // Work A first: nothing is worked yet, so the order does not change.
    sessions
        .set_worked_station(session_id, Some(ids[0]), Some(actor), at + 1_000)
        .await
        .expect("worked-station");

    // Moving the cursor to B marks A worked — A must sink below B and C.
    let applied = match sessions
        .set_worked_station(session_id, Some(ids[1]), Some(actor), at + 2_000)
        .await
        .expect("worked-station")
    {
        WorkedStationOutcome::Set(applied) => applied,
        other => panic!("expected Set, got {other:?}"),
    };
    assert_eq!(applied.set.body.kind(), "station.worked-set");
    let reordered = applied
        .reordered
        .as_ref()
        .expect("the follow-on permutation");
    assert_eq!(reordered.body.kind(), "roster.reordered");
    assert_eq!(
        reordered.seq,
        applied.set.seq + 1,
        "both appends ride one transaction at consecutive seqs"
    );
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[1], ids[2], ids[0]]
    );
}

#[tokio::test]
async fn a_second_round_does_not_sink_the_station_the_ncs_is_working() {
    // `worked` is MONOTONIC, so on round 2
    // the entry holding the cursor is itself worked. The permutation the command
    // boundary emits must still leave that station in the unworked group — the
    // console files the trailing worked run into a collapsed disclosure, so a
    // sunk cursor is a station being worked that nobody can see.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, session_id, actor, ids) = seed_live_roster(
        &pool,
        "sink-round2@example.com",
        &["W1AAA", "W1BBB", "W1CCC"],
    )
    .await;
    let at = now_millis();
    sessions
        .set_roster_order_mode(
            session_id,
            netroll_domain::fold::RosterOrderMode::WorkedSink,
            Some(actor),
            at,
        )
        .await
        .expect("mode");
    // Round 1: work A, then B — which leaves A behind and sinks it.
    sessions
        .set_worked_station(session_id, Some(ids[0]), Some(actor), at + 1_000)
        .await
        .expect("worked-station");
    sessions
        .set_worked_station(session_id, Some(ids[1]), Some(actor), at + 2_000)
        .await
        .expect("worked-station");
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[1], ids[2], ids[0]],
        "round 1 sinks A below the stations still waiting"
    );

    // Round 2: the NCS calls A back. B is left behind and sinks; A re-holds the
    // cursor and stays with the unworked group, below C which still awaits it.
    sessions
        .set_worked_station(session_id, Some(ids[0]), Some(actor), at + 3_000)
        .await
        .expect("worked-station");
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[2], ids[0], ids[1]]
    );

    let folded = netroll_domain::fold::replay(
        &log.events_since(session_id, 0).await.expect("read the log"),
        0,
    );
    assert_eq!(folded.working_check_in_id, Some(ids[0]));
    let worked_tail: Vec<uuid::Uuid> = folded
        .roster
        .iter()
        .rev()
        .take_while(|entry| entry.worked && folded.working_check_in_id != Some(entry.check_in_id))
        .map(|entry| entry.check_in_id)
        .collect();
    assert_eq!(
        worked_tail,
        vec![ids[1]],
        "only the station actually left behind this round sinks"
    );
}

#[tokio::test]
async fn set_worked_station_with_worked_sink_off_still_appends_exactly_one_event() {
    // With the mode OFF — every existing and every new session — the shipped
    // behaviour is byte-identical.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, session_id, actor, ids) =
        seed_live_roster(&pool, "sink-b@example.com", &["W1AAA", "W1BBB", "W1CCC"]).await;
    let at = now_millis();
    sessions
        .set_worked_station(session_id, Some(ids[0]), Some(actor), at)
        .await
        .expect("worked-station");
    let applied = match sessions
        .set_worked_station(session_id, Some(ids[1]), Some(actor), at + 1_000)
        .await
        .expect("worked-station")
    {
        WorkedStationOutcome::Set(applied) => applied,
        other => panic!("expected Set, got {other:?}"),
    };
    assert!(applied.reordered.is_none(), "no permutation with mode off");
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[0], ids[1], ids[2]],
        "the roster order is untouched by a cursor move with the mode off"
    );
}

#[tokio::test]
async fn set_worked_station_under_worked_sink_appends_nothing_extra_when_the_order_holds() {
    // No permutation is appended when the computed order equals the current
    // one — the reorder command's own rollback-on-unchanged guard, reused.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, session_id, actor, ids) =
        seed_live_roster(&pool, "sink-c@example.com", &["W1AAA", "W1BBB", "W1CCC"]).await;
    let at = now_millis();
    sessions
        .set_roster_order_mode(
            session_id,
            netroll_domain::fold::RosterOrderMode::WorkedSink,
            Some(actor),
            at,
        )
        .await
        .expect("mode");
    let applied = match sessions
        .set_worked_station(session_id, Some(ids[0]), Some(actor), at + 1_000)
        .await
        .expect("worked-station")
    {
        WorkedStationOutcome::Set(applied) => applied,
        other => panic!("expected Set, got {other:?}"),
    };
    assert!(
        applied.reordered.is_none(),
        "nothing has been worked yet, so the order is already correct"
    );
    assert_eq!(
        event_kinds(&log, session_id).await,
        vec![
            "session.started",
            "checkin.added",
            "checkin.added",
            "checkin.added",
            "roster.order-mode-set",
            "station.worked-set",
        ]
    );
}

#[tokio::test]
async fn add_check_in_under_worked_sink_lands_at_the_bottom_of_the_unworked_group() {
    // The check-in row is the trigger that makes or breaks the feature: the fold
    // appends a new check-in at the END, i.e. below the worked block, so the
    // command boundary must re-sink it.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, session_id, actor, ids) =
        seed_live_roster(&pool, "sink-d@example.com", &["W1AAA", "W1BBB", "W1CCC"]).await;
    let at = now_millis();
    sessions
        .set_roster_order_mode(
            session_id,
            netroll_domain::fold::RosterOrderMode::WorkedSink,
            Some(actor),
            at,
        )
        .await
        .expect("mode");
    sessions
        .set_worked_station(session_id, Some(ids[0]), Some(actor), at + 1_000)
        .await
        .expect("worked-station");
    sessions
        .set_worked_station(session_id, Some(ids[1]), Some(actor), at + 2_000)
        .await
        .expect("worked-station");
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[1], ids[2], ids[0]]
    );

    let late = uuid::Uuid::now_v7();
    let applied = match sessions
        .add_check_in(
            session_id,
            &parse_callsign("W1DDD").expect("valid"),
            late,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            at + 3_000,
        )
        .await
        .expect("add")
    {
        AddCheckInOutcome::Added(applied) => applied,
        other => panic!("expected Added, got {other:?}"),
    };
    let reordered = applied
        .reordered
        .as_ref()
        .expect("the follow-on permutation");
    assert_eq!(reordered.seq, applied.added.seq + 1);
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[1], ids[2], late, ids[0]],
        "the late arrival lands at the bottom of the UNWORKED group, not below the worked block"
    );
}

#[tokio::test]
async fn reorder_roster_under_worked_sink_composes_precedence_within_each_group() {
    // Worked-sink is the OUTER key; a bare precedence sort would lift
    // the worked emergency station back to the top of the whole roster.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, session_id, actor, ids) = seed_live_roster(
        &pool,
        "sink-e@example.com",
        &["W1AAA", "W1BBB", "W1CCC", "W1DDD"],
    )
    .await;
    let at = now_millis();
    // A -> emergency, C -> priority; B and D stay routine.
    sessions
        .edit_check_in(
            session_id,
            ids[0],
            1,
            &parse_callsign("W1AAA").expect("valid"),
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Emergency,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            at,
        )
        .await
        .expect("edit");
    sessions
        .edit_check_in(
            session_id,
            ids[2],
            1,
            &parse_callsign("W1CCC").expect("valid"),
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Priority,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            at + 500,
        )
        .await
        .expect("edit");
    sessions
        .set_roster_order_mode(
            session_id,
            netroll_domain::fold::RosterOrderMode::WorkedSink,
            Some(actor),
            at + 1_000,
        )
        .await
        .expect("mode");
    sessions
        .set_worked_station(session_id, Some(ids[0]), Some(actor), at + 2_000)
        .await
        .expect("worked-station");
    sessions
        .set_worked_station(session_id, Some(ids[1]), Some(actor), at + 3_000)
        .await
        .expect("worked-station");
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[1], ids[2], ids[3], ids[0]]
    );

    let outcome = sessions
        .reorder_roster(session_id, Some(actor), at + 4_000)
        .await
        .expect("reorder");
    assert!(matches!(outcome, ReorderOutcome::Reordered(_)));
    assert_eq!(
        folded_order(&log, session_id).await,
        // Unworked group by precedence (C priority, then B and D routine in their
        // prior relative order), then the worked group (A, emergency) LAST.
        vec![ids[2], ids[1], ids[3], ids[0]]
    );
}

#[tokio::test]
async fn set_roster_order_mode_enabling_worked_sink_sinks_the_worked_stations_in_one_transaction() {
    // Enabling the mode is itself a durable, seq'd event, and it
    // stable-partitions the CURRENT order in the same transaction.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, session_id, actor, ids) =
        seed_live_roster(&pool, "sink-f@example.com", &["W1AAA", "W1BBB", "W1CCC"]).await;
    let at = now_millis();
    sessions
        .set_worked_station(session_id, Some(ids[0]), Some(actor), at)
        .await
        .expect("worked-station");
    sessions
        .set_worked_station(session_id, Some(ids[1]), Some(actor), at + 1_000)
        .await
        .expect("worked-station");
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[0], ids[1], ids[2]],
        "with the mode off the cursor move left the order alone"
    );

    let applied = match sessions
        .set_roster_order_mode(
            session_id,
            netroll_domain::fold::RosterOrderMode::WorkedSink,
            Some(actor),
            at + 2_000,
        )
        .await
        .expect("mode")
    {
        OrderModeOutcome::Set(applied) => applied,
        other => panic!("expected Set, got {other:?}"),
    };
    assert_eq!(applied.mode_set.body.kind(), "roster.order-mode-set");
    let reordered = applied
        .reordered
        .as_ref()
        .expect("the follow-on permutation");
    assert_eq!(reordered.seq, applied.mode_set.seq + 1);
    assert_eq!(
        folded_order(&log, session_id).await,
        vec![ids[1], ids[2], ids[0]]
    );

    // Setting the same mode again is a no-op: nothing appended, no churn.
    let before = log.events_since(session_id, 0).await.expect("log").len();
    let repeat = sessions
        .set_roster_order_mode(
            session_id,
            netroll_domain::fold::RosterOrderMode::WorkedSink,
            Some(actor),
            at + 3_000,
        )
        .await
        .expect("mode");
    assert!(matches!(repeat, OrderModeOutcome::NoOp));
    let after = log.events_since(session_id, 0).await.expect("log").len();
    assert_eq!(before, after);
}

#[tokio::test]
async fn set_net_note_appends_and_folds_the_note_and_is_a_noop_when_unchanged() {
    // Set_net_note appends session.note-set and folds the note;
    // setting the SAME value again is a NoOp with no second append.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "note-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let note = netroll_domain::check_in::parse_note("weekly traffic net")
        .expect("valid")
        .expect("non-blank");
    let outcome = sessions
        .set_net_note(row.id, Some(note.clone()), Some(actor), started_at + 1_000)
        .await
        .expect("net-note call");
    assert!(matches!(outcome, NoteOutcome::Set(_)));
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(
        netroll_domain::fold::replay(&events, 0)
            .net_note
            .as_ref()
            .map(|n| n.as_str()),
        Some("weekly traffic net")
    );

    // Setting the SAME note again short-circuits with NoOp (no churn).
    let before = log.events_since(row.id, 0).await.expect("log").len();
    let again = sessions
        .set_net_note(row.id, Some(note), Some(actor), started_at + 2_000)
        .await
        .expect("net-note call");
    assert!(matches!(again, NoteOutcome::NoOp));
    let after = log.events_since(row.id, 0).await.expect("log").len();
    assert_eq!(before, after, "no append for an unchanged note");
}

#[tokio::test]
async fn remove_check_in_appends_a_tombstone_and_drops_the_row() {
    // Checkin.removed drops the projected row while the log
    // retains the full history (the append-only tombstone).
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "remove-owner@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add");

    let outcome = sessions
        .remove_check_in(row.id, check_in_id, 1, Some(actor), started_at + 2_000)
        .await
        .expect("remove call");
    let event = match outcome {
        EditCheckInOutcome::Applied(event) => event,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(event.body.kind(), "checkin.removed");

    // The projected roster is empty, but BOTH events remain in the durable log.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 3); // started + added + removed
    let state = netroll_domain::fold::replay(&events, 0);
    assert!(state.roster.is_empty());
}

#[tokio::test]
async fn moderate_block_on_a_self_entry_removes_the_row_and_blocks_the_account() {
    // Moderating a self-checked-in account with block=true
    // appends BOTH checkin.removed AND station.blocked in one transaction; the
    // row drops out of the roster and the account joins the fold-derived blocklist.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "mod-block-owner@example.com").await;
    let started_at = now_millis();
    let ncs = seed_account(&pool, "mod-ncs@example.com").await;
    let participant = seed_account(&pool, "mod-participant@example.com").await;
    let row = match sessions
        .start(&definition, Some(ncs), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    // A SELF check-in: source SelfService, actor is the participant's own account.
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(participant),
            started_at + 1_000,
        )
        .await
        .expect("self add");

    let outcome = sessions
        .moderate_check_in(row.id, check_in_id, true, 1, Some(ncs), started_at + 2_000)
        .await
        .expect("moderate call");
    let (removed, blocked) = match outcome {
        ModerateOutcome::Applied(applied) => (applied.removed, applied.blocked),
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(removed.body.kind(), "checkin.removed");
    let blocked = blocked.expect("a self entry carries an account to block");
    assert_eq!(blocked.body.kind(), "station.blocked");

    // Both events landed in the durable log; the fold drops the row AND records
    // the block against the participant's account.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 4); // started + added + removed + blocked
    let state = netroll_domain::fold::replay(&events, 0);
    assert!(state.roster.is_empty());
    assert_eq!(state.blocked_account_ids, vec![participant]);
}

#[tokio::test]
async fn moderate_block_on_an_account_less_entry_appends_neither_and_refuses() {
    // A block explicitly requested against an account-less
    // staff-logged entry is refused (NothingToBlock) and the removal is NOT
    // performed — all-or-nothing.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "mod-noacct-owner@example.com").await;
    let started_at = now_millis();
    let ncs = seed_account(&pool, "mod-noacct-ncs@example.com").await;
    let row = match sessions
        .start(&definition, Some(ncs), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    // A STAFF-logged, account-less entry: source Staff, no participant account.
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            None,
            started_at + 1_000,
        )
        .await
        .expect("staff add");

    let outcome = sessions
        .moderate_check_in(row.id, check_in_id, true, 1, Some(ncs), started_at + 2_000)
        .await
        .expect("moderate call");
    assert!(matches!(outcome, ModerateOutcome::NothingToBlock));

    // Neither event was appended: the log holds only started + added, and the
    // row is STILL on the roster (the removal did not partially succeed).
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 2); // started + added only
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster.len(), 1);
    assert!(state.blocked_account_ids.is_empty());
}

#[tokio::test]
async fn moderate_remove_only_appends_only_checkin_removed() {
    // A remove-only moderation (block=false) drops the row via a
    // single checkin.removed and records no block.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) =
        seed_definition(&pool, "mod-removeonly-owner@example.com").await;
    let started_at = now_millis();
    let ncs = seed_account(&pool, "mod-removeonly-ncs@example.com").await;
    let participant = seed_account(&pool, "mod-removeonly-part@example.com").await;
    let row = match sessions
        .start(&definition, Some(ncs), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(participant),
            started_at + 1_000,
        )
        .await
        .expect("self add");

    let outcome = sessions
        .moderate_check_in(row.id, check_in_id, false, 1, Some(ncs), started_at + 2_000)
        .await
        .expect("moderate call");
    let (removed, blocked) = match outcome {
        ModerateOutcome::Applied(applied) => (applied.removed, applied.blocked),
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(removed.body.kind(), "checkin.removed");
    assert!(blocked.is_none(), "block was not requested");

    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 3); // started + added + removed
    let state = netroll_domain::fold::replay(&events, 0);
    assert!(state.roster.is_empty());
    assert!(state.blocked_account_ids.is_empty());
}

#[tokio::test]
async fn moderate_with_a_stale_version_refuses_and_appends_nothing() {
    // A version-CAS miss refuses without appending either event.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "mod-stale-owner@example.com").await;
    let started_at = now_millis();
    let ncs = seed_account(&pool, "mod-stale-ncs@example.com").await;
    let participant = seed_account(&pool, "mod-stale-part@example.com").await;
    let row = match sessions
        .start(&definition, Some(ncs), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(participant),
            started_at + 1_000,
        )
        .await
        .expect("self add");

    // Expected version 2, but the entry is at version 1 — a stale CAS.
    let outcome = sessions
        .moderate_check_in(row.id, check_in_id, true, 2, Some(ncs), started_at + 2_000)
        .await
        .expect("moderate call");
    assert!(matches!(outcome, ModerateOutcome::StaleVersion));
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 2); // started + added only — nothing appended
}

#[tokio::test]
async fn moderating_a_closed_session_returns_not_live() {
    // `moderate_check_in` shares the same
    // `guard_live_in_tx` gate as every other guarded write — a session no
    // longer `live` at write time refuses with `NotLive` and appends nothing,
    // mirroring the precedent `editing_a_check_in_on_a_closed_session_is_refused_not_live`
    // sets for the sibling edit path (previously untested for moderation).
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "mod-closed-owner@example.com").await;
    let started_at = now_millis();
    let ncs = seed_account(&pool, "mod-closed-ncs@example.com").await;
    let row = match sessions
        .start(&definition, Some(ncs), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            None,
            started_at + 1_000,
        )
        .await
        .expect("staff add");
    sessions
        .close(row.id, started_at + 2_000, Some(ncs))
        .await
        .expect("close call");

    let outcome = sessions
        .moderate_check_in(row.id, check_in_id, false, 1, Some(ncs), started_at + 3_000)
        .await
        .expect("moderate call");
    assert!(matches!(outcome, ModerateOutcome::NotLive));

    // Nothing appended beyond started + added + closed.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 3);
}

#[tokio::test]
async fn add_check_in_for_a_blocked_account_is_refused_inside_the_add_check_in_transaction() {
    // Closing a TOCTOU window: the HTTP handler's own
    // blocklist pre-check reads an OUT-OF-transaction fold, which is not atomic
    // with the later `add_check_in` write — a block that commits in the gap
    // between the two could otherwise slip a check-in through. This test calls
    // the ADAPTER directly, bypassing the HTTP handler's pre-check entirely, to
    // prove the guard is real INSIDE `add_check_in`'s own guarded transaction —
    // not merely enforced one layer up.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "mod-race-owner@example.com").await;
    let started_at = now_millis();
    let ncs = seed_account(&pool, "mod-race-ncs@example.com").await;
    let participant = seed_account(&pool, "mod-race-part@example.com").await;
    let row = match sessions
        .start(&definition, Some(ncs), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(participant),
            started_at + 1_000,
        )
        .await
        .expect("self add");

    // The NCS blocks the account (also removes the current row).
    let outcome = sessions
        .moderate_check_in(row.id, check_in_id, true, 1, Some(ncs), started_at + 2_000)
        .await
        .expect("moderate call");
    assert!(matches!(outcome, ModerateOutcome::Applied(_)));

    // The SAME account re-attempts a self check-in DIRECTLY via the adapter —
    // no HTTP handler in the loop, so any handler-level pre-check is
    // completely bypassed. The transaction's own re-fold must still catch it.
    let new_check_in_id = uuid::Uuid::now_v7();
    let outcome = sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            new_check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(participant),
            started_at + 3_000,
        )
        .await
        .expect("add call");
    assert!(matches!(outcome, AddCheckInOutcome::AccountBlocked));

    // No new event was appended for the refused attempt.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    assert_eq!(events.len(), 4); // started + added + removed + blocked, nothing more
}

#[tokio::test]
async fn editing_a_check_in_on_a_closed_session_is_refused_not_live() {
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) = seed_definition(&pool, "edit-closed@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let check_in_id = uuid::Uuid::now_v7();
    sessions
        .add_check_in(
            row.id,
            &parse_callsign("W1AW").expect("valid"),
            check_in_id,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add");
    sessions
        .close(row.id, started_at + 2_000, Some(actor))
        .await
        .expect("close");

    let outcome = sessions
        .edit_check_in(
            row.id,
            check_in_id,
            1,
            &parse_callsign("W1AX").expect("valid"),
            None,
            None,
            None,
            None,
            StayingStatus::InAndOut,
            Precedence::Routine,
            None,
            None,
            None,
            None,
            None,
            Some(actor),
            started_at + 3_000,
        )
        .await
        .expect("edit call");
    assert!(matches!(outcome, EditCheckInOutcome::NotLive));
}

#[tokio::test]
async fn add_check_in_round_trips_signal_report_and_staying_through_the_fold() {
    // A report + staying-for-comments persist in the JSONB
    // payload and fold back onto the roster entry — end-to-end through the
    // adapter's serde seam.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "checkin-report@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let check_in_id = uuid::Uuid::now_v7();
    let callsign = parse_callsign("W1AW").expect("valid callsign");
    let report = parse_signal_report("599")
        .expect("valid report")
        .expect("non-blank");
    sessions
        .add_check_in(
            row.id,
            &callsign,
            check_in_id,
            None,
            Some(&report),
            StayingStatus::StayingForComments,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add check-in");

    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster.len(), 1);
    assert_eq!(
        state.roster[0].signal_report.as_ref().map(|r| r.as_str()),
        Some("599")
    );
    assert_eq!(state.roster[0].staying, StayingStatus::StayingForComments);

    // The stored JSONB carries the camelCase signalReport + the kebab staying
    // token — the wire seam is genuinely camelCase (not just "it decoded").
    let raw: String = sqlx::query_scalar(
        "SELECT payload::text FROM session_events WHERE session_id = $1 AND seq = 2",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .expect("read raw payload");
    assert!(
        raw.contains("signalReport"),
        "expected signalReport key, got {raw}"
    );
    assert!(
        raw.contains("staying-for-comments"),
        "expected staying token, got {raw}"
    );
}

#[tokio::test]
async fn add_check_in_without_a_report_omits_the_key_and_defaults_staying() {
    // Regression: a callsign-only add (no report, default
    // staying) omits signalReport from the JSON (the omit-optional rule) and
    // folds to in-and-out — the callsign-only shape is preserved.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "checkin-noreport@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let callsign = parse_callsign("W1AW").expect("valid callsign");
    sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add check-in");

    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster[0].signal_report, None);
    assert_eq!(state.roster[0].staying, StayingStatus::InAndOut);

    let raw: String = sqlx::query_scalar(
        "SELECT payload::text FROM session_events WHERE session_id = $1 AND seq = 2",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .expect("read raw payload");
    assert!(
        !raw.contains("signalReport"),
        "signalReport is omitted when absent, got {raw}"
    );
}

#[tokio::test]
async fn a_historical_field_less_checkin_payload_folds_to_defaults() {
    // A checkin.added row written before those fields has no
    // signalReport/staying keys in its JSONB. On read it MUST decode to
    // staying = in-and-out, signal_report = None — never a decode error (the
    // additive-compat replay guarantee). Simulated by stripping both
    // keys from a freshly-written payload.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "checkin-legacy@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let callsign = parse_callsign("W1AW").expect("valid callsign");
    sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            Some(
                &parse_signal_report("599")
                    .expect("valid")
                    .expect("non-blank"),
            ),
            StayingStatus::StayingForComments,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("add check-in");

    // Strip the two keys to reproduce a legacy field-less payload.
    sqlx::query(
        "UPDATE session_events SET payload = (payload - 'staying' - 'signalReport')
         WHERE session_id = $1 AND seq = 2",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .expect("strip 4.3 keys");

    // The decode does not fail and folds to the defaults.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster.len(), 1);
    assert_eq!(state.roster[0].signal_report, None);
    assert_eq!(state.roster[0].staying, StayingStatus::InAndOut);
}

#[tokio::test]
async fn add_check_in_with_a_duplicate_client_event_id_is_idempotent_and_appends_no_second_event() {
    // A second add with the SAME (session, clientEventId) — even
    // with a fresh check_in_id — hits the partial unique idempotency index and
    // resolves to Duplicate, appending no phantom second checkin.added. The
    // in-txn last_seq bump is reverted (gapless), so the session stays at seq 2.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "checkin-dup@example.com").await;
    let started_at = now_millis();
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let row = match sessions
        .start(&definition, Some(actor), started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let callsign = parse_callsign("W1AW").expect("valid callsign");
    let client_event_id = uuid::Uuid::now_v7();

    let first = sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            Some(client_event_id),
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 1_000,
        )
        .await
        .expect("first add");
    assert!(
        matches!(first, AddCheckInOutcome::Added(_)),
        "the first optimistic add is Added, got {first:?}"
    );

    // A DISTINCT check_in_id but the SAME clientEventId — the double-submit shape.
    let second = sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            Some(client_event_id),
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            started_at + 2_000,
        )
        .await
        .expect("second add returns an outcome, not a db error");
    assert!(
        matches!(second, AddCheckInOutcome::Duplicate),
        "the second add with the same clientEventId is Duplicate, got {second:?}"
    );

    // Exactly one checkin.added; the log folds to a single roster row; seq is 2.
    let events = log.events_since(row.id, 0).await.expect("read the log");
    let checkins = events
        .iter()
        .filter(|e| matches!(e.body, SessionEventBody::CheckinAdded { .. }))
        .count();
    assert_eq!(
        checkins, 1,
        "no phantom second checkin.added for the dup id"
    );
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster.len(), 1);
    let found = sessions
        .find(row.id)
        .await
        .expect("find")
        .expect("session exists");
    assert_eq!(
        found.last_seq, 2,
        "the reverted dup append left seq gapless at 2"
    );
}

#[tokio::test]
async fn add_check_in_on_a_closed_session_is_not_live_and_appends_nothing() {
    // The atomic `AND lifecycle = 'live'` guard is the
    // real authority — a closed session refuses the add with no phantom event.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "checkin-closed@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, None, started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    match sessions
        .close(row.id, started_at + 1_000, None)
        .await
        .expect("close")
    {
        CloseOutcome::Closed(_) => {}
        other => panic!("expected Closed, got {other:?}"),
    };

    let callsign = parse_callsign("W1AW").expect("valid callsign");
    let outcome = sessions
        .add_check_in(
            row.id,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            None,
            started_at + 2_000,
        )
        .await
        .expect("add_check_in returns an outcome, not a db error, on a closed session");
    assert!(
        matches!(outcome, AddCheckInOutcome::NotLive),
        "a closed session refuses the add as NotLive, got {outcome:?}"
    );

    let events = log.events_since(row.id, 0).await.expect("read the log");
    let checkins = events
        .iter()
        .filter(|e| matches!(e.body, SessionEventBody::CheckinAdded { .. }))
        .count();
    assert_eq!(checkins, 0, "no phantom checkin.added on a closed session");
}

#[tokio::test]
async fn add_check_in_on_a_missing_session_is_the_missing_outcome() {
    let (_container, pool) = migrated_pool().await;
    let sessions = NetSessionRepo::new(pool);
    let callsign = parse_callsign("W1AW").expect("valid callsign");
    let outcome = sessions
        .add_check_in(
            uuid::Uuid::now_v7(),
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            None,
            now_millis(),
        )
        .await
        .expect("add_check_in returns an outcome, not a db error, for a missing session");
    assert!(
        matches!(outcome, AddCheckInOutcome::Missing),
        "adding a check-in to a nonexistent session surfaces Missing, got {outcome:?}"
    );
}

#[tokio::test]
async fn two_check_ins_append_two_rows_and_a_repeat_id_never_grows_the_roster() {
    // Two distinct check_in_ids fold to a two-row roster;
    // a duplicate check_in_id at an advancing seq is inert in the fold (the
    // shipped entity-level dedupe — no new dedupe logic).
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "checkin-dupe@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, None, started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let id_a = uuid::Uuid::now_v7();
    let id_b = uuid::Uuid::now_v7();
    let call_a = parse_callsign("n1ale").expect("valid callsign");
    let call_b = parse_callsign("k2xyz").expect("valid callsign");
    sessions
        .add_check_in(
            row.id,
            &call_a,
            id_a,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            None,
            started_at + 1_000,
        )
        .await
        .expect("first check-in");
    sessions
        .add_check_in(
            row.id,
            &call_b,
            id_b,
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            None,
            started_at + 2_000,
        )
        .await
        .expect("second check-in");

    let events = log.events_since(row.id, 0).await.expect("read the log");
    let state = netroll_domain::fold::replay(&events, 0);
    assert_eq!(state.roster.len(), 2, "two distinct check-ins, two rows");

    // Idempotency confirm: folding a duplicate of id_a at an advancing seq
    // leaves the roster length unchanged (the shipped dedupe, not new logic).
    let dup = SessionEvent {
        seq: 99,
        actor_id: None,
        at: started_at + 3_000,
        body: SessionEventBody::CheckinAdded {
            check_in_id: id_a,
            callsign: call_a.clone(),
            client_event_id: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            name: None,
            location: None,
            grid: None,
            source: CheckInSource::Staff,
            via: None,
            relayed_by: None,
        },
    };
    let after_dup = netroll_domain::fold::fold(state, &dup);
    assert_eq!(
        after_dup.roster.len(),
        2,
        "a repeated check_in_id is inert in the fold — the roster does not grow"
    );
}

#[tokio::test]
async fn closing_a_missing_session_is_the_missing_outcome() {
    let (_container, pool) = migrated_pool().await;
    let sessions = NetSessionRepo::new(pool);
    let outcome = sessions
        .close(uuid::Uuid::now_v7(), now_millis(), None)
        .await
        .expect("close returns an outcome, not a db error, for a missing session");
    assert!(
        matches!(outcome, CloseOutcome::Missing),
        "closing a nonexistent session surfaces Missing, got {outcome:?}"
    );
}

#[tokio::test]
async fn starting_a_session_for_an_archived_definition_is_refused_by_the_adapter() {
    // The adapter's own guarded INSERT
    // must refuse an archived definition on its OWN authority, independent of
    // whatever pre-flight check the HTTP handler layers on top — this is what
    // closes the TOCTOU race where a definition archives between the
    // handler's read and the write.
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) = seed_definition(&pool, "archived-start@example.com").await;
    let defs = NetDefinitionRepo::new(pool.clone());
    assert!(
        defs.archive(definition.id, now_millis())
            .await
            .expect("archive"),
        "archive succeeds"
    );

    let outcome = sessions
        .start(&definition, None, now_millis())
        .await
        .expect("start returns an outcome, not a db error, for an archived definition");
    assert!(
        matches!(outcome, StartOutcome::DefinitionArchived),
        "expected DefinitionArchived, got {outcome:?}"
    );
    assert_eq!(
        session_count_for(&pool, definition.id).await,
        0,
        "no session row is left behind by a refused start"
    );
}

#[tokio::test]
async fn a_second_start_for_an_already_live_definition_is_refused_as_already_live() {
    // A definition may have at
    // most one concurrently-live session — the partial unique index
    // (net_sessions_one_live_per_definition) is the enforcement, not just a
    // documented forward item.
    let (_container, pool) = migrated_pool().await;
    let (sessions, _log, definition) = seed_definition(&pool, "double-start@example.com").await;

    let first = sessions
        .start(&definition, None, now_millis())
        .await
        .expect("first start");
    assert!(matches!(first, StartOutcome::Started(_)));

    let second = sessions
        .start(&definition, None, now_millis())
        .await
        .expect("second start returns an outcome, not a db error");
    assert!(
        matches!(second, StartOutcome::AlreadyLive),
        "expected AlreadyLive, got {second:?}"
    );
    assert_eq!(
        session_count_for(&pool, definition.id).await,
        1,
        "the refused second start leaves exactly one session row"
    );
}

#[tokio::test]
async fn concurrent_closes_of_the_same_session_only_one_succeeds_and_only_one_event_is_appended() {
    // A check-then-act race between the
    // HTTP handler's stale pre-flight lifecycle read and the actual write
    // could otherwise let two racing close requests both append
    // `session.closed`. The guarded UPDATE (`WHERE lifecycle = 'live'`)
    // closes this: under real concurrent access, exactly one request's close
    // succeeds and the other observes `NotLive` with no event appended.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "concurrent-close@example.com").await;
    let started_at = now_millis();
    let row = match sessions
        .start(&definition, None, started_at)
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let session_id = row.id;
    let a = {
        let sessions = sessions.clone();
        tokio::spawn(async move {
            sessions
                .close(session_id, started_at + 1_000, None)
                .await
                .expect("close a")
        })
    };
    let b = {
        let sessions = sessions.clone();
        tokio::spawn(async move {
            sessions
                .close(session_id, started_at + 2_000, None)
                .await
                .expect("close b")
        })
    };
    let (outcome_a, outcome_b) = (
        a.await.expect("task a joins"),
        b.await.expect("task b joins"),
    );

    let outcomes = [&outcome_a, &outcome_b];
    let closed_count = outcomes
        .iter()
        .filter(|o| matches!(o, CloseOutcome::Closed(_)))
        .count();
    let not_live_count = outcomes
        .iter()
        .filter(|o| matches!(o, CloseOutcome::NotLive))
        .count();
    assert_eq!(
        closed_count, 1,
        "exactly one of the two concurrent closes wins, got {outcome_a:?} / {outcome_b:?}"
    );
    assert_eq!(
        not_live_count, 1,
        "the other concurrent close loses the race as NotLive, got {outcome_a:?} / {outcome_b:?}"
    );

    let events = log.events_since(session_id, 0).await.expect("read the log");
    let closed_events = events
        .iter()
        .filter(|e| matches!(e.body, SessionEventBody::SessionClosed))
        .count();
    assert_eq!(
        closed_events, 1,
        "only ONE session.closed event was appended despite two concurrent close calls"
    );
}

#[tokio::test]
async fn create_and_append_public_methods_are_unchanged_by_the_tx_core_refactor() {
    // Regression: the *_in_tx refactor must be transparent — create still writes
    // a live row at last_seq 0, and a subsequent standalone append still assigns
    // seq 1 exactly as before.
    let (_container, pool) = migrated_pool().await;
    let (sessions, log, definition) = seed_definition(&pool, "regress-owner@example.com").await;

    let created = sessions
        .create(&definition, now_millis())
        .await
        .expect("create session");
    assert_eq!(created.lifecycle, SessionLifecycle::Live);
    assert_eq!(created.last_seq, 0, "create alone writes no events");

    let event = log
        .append(
            created.id,
            &SessionEventBody::SessionStarted {
                definition_id: definition.id,
                definition_version: definition.definition_version,
            },
            None,
            now_millis(),
        )
        .await
        .expect("append");
    assert_eq!(event.seq, 1, "the first standalone append is seq 1");

    let found = sessions
        .find(created.id)
        .await
        .expect("find")
        .expect("exists");
    assert_eq!(found.last_seq, 1);
}

/// Seeds a definition (owned by a fresh account) + a live session, plus a
/// separate staff account to grant roles to. Returns `(session_id,
/// staff_account_id)`.
async fn seed_role_fixture(pool: &PgPool, suffix: &str) -> (uuid::Uuid, uuid::Uuid) {
    let defs = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(pool, &format!("role-owner-{suffix}@example.com")).await;
    let definition = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            &format!("tok-{suffix}"),
            now_millis(),
        )
        .await
        .expect("create definition");
    let sessions = NetSessionRepo::new(pool.clone());
    let session = sessions
        .create(&definition, now_millis())
        .await
        .expect("create session");
    let staff = seed_account(pool, &format!("role-staff-{suffix}@example.com")).await;
    (session.id, staff)
}

#[tokio::test]
async fn grant_then_find_role_round_trips_the_granted_role() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let (session, staff) = seed_role_fixture(&pool, "roundtrip").await;

    roles
        .grant(session, staff, Role::Logger, staff)
        .await
        .expect("grant");

    let found = roles.find_role(session, staff).await.expect("find");
    assert_eq!(found, Some(Role::Logger));
}

#[tokio::test]
async fn re_granting_overwrites_the_existing_role() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let (session, staff) = seed_role_fixture(&pool, "upsert").await;

    roles
        .grant(session, staff, Role::Relay, staff)
        .await
        .expect("first grant");
    roles
        .grant(session, staff, Role::NetControl, staff)
        .await
        .expect("re-grant upserts");

    assert_eq!(
        roles.find_role(session, staff).await.expect("find"),
        Some(Role::NetControl),
        "the re-grant overwrote the earlier role, not inserted a second row"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM net_session_roles WHERE account_id = $1")
            .bind(staff)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(count, 1, "the (session, account) key kept exactly one row");
}

#[tokio::test]
async fn find_role_misses_return_none() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let (session, staff) = seed_role_fixture(&pool, "miss").await;

    // No grant yet.
    assert_eq!(roles.find_role(session, staff).await.expect("find"), None);

    // An ungranted stranger on a session with a grant still misses.
    let stranger = seed_account(&pool, "stranger-miss@example.com").await;
    roles
        .grant(session, staff, Role::Logger, staff)
        .await
        .expect("grant");
    assert_eq!(
        roles.find_role(session, stranger).await.expect("find"),
        None
    );
}

#[tokio::test]
async fn revoke_removes_a_grant_and_reports_whether_one_existed() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let (session, staff) = seed_role_fixture(&pool, "revoke").await;

    roles
        .grant(session, staff, Role::Logger, staff)
        .await
        .expect("grant");
    assert_eq!(
        roles.revoke(session, staff).await.expect("revoke"),
        RevokeOutcome::Revoked
    );
    assert_eq!(roles.find_role(session, staff).await.expect("find"), None);

    // A second revoke of the now-absent grant is NotAMember (the API maps this
    // to a 404, symmetric with remove_owner).
    assert_eq!(
        roles.revoke(session, staff).await.expect("revoke"),
        RevokeOutcome::NotAMember
    );
}

#[tokio::test]
async fn revoke_if_role_only_deletes_a_matching_role_and_reports_a_mismatch() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let (session, staff) = seed_role_fixture(&pool, "revoke-if-role").await;

    roles
        .grant(session, staff, Role::Logger, staff)
        .await
        .expect("grant");

    // A stale expectation (Relay) does not match the stored role (Logger) —
    // this is the TOCTOU-closing compare-and-delete (review finding): a
    // concurrent grant could have changed the row after the caller's
    // authorization decision was made from an earlier read.
    assert_eq!(
        roles
            .revoke_if_role(session, staff, Role::Relay)
            .await
            .expect("revoke_if_role"),
        RevokeOutcome::NotAMember,
        "a mismatched expected role must not delete the row"
    );
    assert_eq!(
        roles.find_role(session, staff).await.expect("find"),
        Some(Role::Logger),
        "the actual grant survives a mismatched compare-and-delete"
    );

    // The matching expectation deletes it.
    assert_eq!(
        roles
            .revoke_if_role(session, staff, Role::Logger)
            .await
            .expect("revoke_if_role"),
        RevokeOutcome::Revoked
    );
    assert_eq!(roles.find_role(session, staff).await.expect("find"), None);
}

#[tokio::test]
async fn a_grant_is_scoped_to_its_own_session() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let (session_a, staff) = seed_role_fixture(&pool, "scope-a").await;
    let (session_b, _other_staff) = seed_role_fixture(&pool, "scope-b").await;

    // Grant Logger on session A only.
    roles
        .grant(session_a, staff, Role::Logger, staff)
        .await
        .expect("grant on A");

    assert_eq!(
        roles.find_role(session_a, staff).await.expect("find A"),
        Some(Role::Logger)
    );
    // The SAME account resolves to no grant on session B — cross-net isolation
    // by construction, no special case.
    assert_eq!(
        roles.find_role(session_b, staff).await.expect("find B"),
        None
    );
}

#[tokio::test]
async fn list_grants_returns_each_grant_with_its_callsign_stably_ordered() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let accounts = AccountRepo::new(pool.clone());
    let (session, first) = seed_role_fixture(&pool, "list-grants").await;
    let now = now_millis();
    accounts
        .set_callsign(first, "W1AW", now)
        .await
        .expect("first callsign");
    // A second grantee WITHOUT a callsign — the join must still return the row,
    // with callsign None (a callsign is a first-class optional).
    let second = seed_account(&pool, "list-grants-second@example.com").await;

    // The grantor (recorded as granted_by) is `first`; grant Relay then Logger.
    roles
        .grant(session, first, Role::Relay, first)
        .await
        .expect("grant first");
    roles
        .grant(session, second, Role::Logger, first)
        .await
        .expect("grant second");

    // Both grants land at effectively the same instant under `DEFAULT now()`,
    // so insertion order alone can't prove the `ORDER BY created_at,
    // account_id` clause is doing the ordering (vs. e.g. an incidental
    // physical/insertion order). Backdate `second`'s `created_at` explicitly
    // BEFORE `first`'s, then assert the returned Vec's actual sequence
    // reverses accordingly — this is the only way to prove the query orders
    // by `created_at` rather than by insertion order ("stably ordered").
    sqlx::query!(
        "UPDATE net_session_roles SET created_at = created_at - interval '2 seconds'
         WHERE net_session_id = $1 AND account_id = $2",
        session,
        second,
    )
    .execute(&pool)
    .await
    .expect("backdate second grant's created_at");

    let grants = roles.list_grants(session).await.expect("list grants");
    assert_eq!(grants.len(), 2, "both explicit grants are listed");

    // `second` now has the EARLIER `created_at`, so it must sort first — this
    // is the assertion the story's "stably ordered" requirement actually
    // needs and the prior HashMap-based check never made.
    assert_eq!(
        grants[0].account_id, second,
        "the backdated grant sorts first by created_at"
    );
    assert_eq!(
        grants[1].account_id, first,
        "the later-created_at grant sorts second"
    );

    let first_grant = &grants[1];
    assert_eq!(first_grant.callsign, Some("W1AW".to_owned()));
    assert_eq!(first_grant.role, Role::Relay);
    assert_eq!(first_grant.granted_by, Some(first));
    let second_grant = &grants[0];
    assert_eq!(
        second_grant.callsign, None,
        "a grantee without a callsign still lists, with callsign None"
    );
    assert_eq!(second_grant.role, Role::Logger);
}

#[tokio::test]
async fn list_grants_breaks_a_created_at_tie_by_account_id() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let (session, first) = seed_role_fixture(&pool, "list-grants-tie").await;
    let second = seed_account(&pool, "list-grants-tie-second@example.com").await;

    roles
        .grant(session, first, Role::Relay, first)
        .await
        .expect("grant first");
    roles
        .grant(session, second, Role::Logger, first)
        .await
        .expect("grant second");

    // Force an exact `created_at` tie between the two grants, so the ONLY
    // thing that can determine order is the `account_id` tiebreaker named in
    // the account-id tiebreaker precedent.
    sqlx::query!(
        "UPDATE net_session_roles SET created_at = now() WHERE net_session_id = $1",
        session,
    )
    .execute(&pool)
    .await
    .expect("force a created_at tie");

    let grants = roles.list_grants(session).await.expect("list grants");
    assert_eq!(grants.len(), 2);
    let expected_first = std::cmp::min(first, second);
    let expected_second = std::cmp::max(first, second);
    assert_eq!(
        grants[0].account_id, expected_first,
        "on a created_at tie, the lower account_id sorts first"
    );
    assert_eq!(grants[1].account_id, expected_second);
}

#[tokio::test]
async fn list_grants_is_empty_for_a_session_with_no_explicit_grants() {
    let (_container, pool) = migrated_pool().await;
    let roles = NetSessionRoleRepo::new(pool.clone());
    let (session, _staff) = seed_role_fixture(&pool, "list-grants-empty").await;

    // The owner is NEVER stored as a grant — a session whose only staff is
    // its derived owner lists ZERO explicit grants.
    let grants = roles.list_grants(session).await.expect("list grants");
    assert_eq!(grants, Vec::<RoleGrant>::new());
}

// --- Control-status guarded writes + freeze + monitor query -----

/// Starts a live session and returns (repo, log, row) — the arrange step for
/// the control-status tests. Builds its OWN uniquely-tokened definition (keyed
/// off `email`) so a single test can start two independent sessions without the
/// link-token unique-index collision the shared `seed_definition` would hit.
async fn start_live_session(
    pool: &PgPool,
    email: &str,
    actor: uuid::Uuid,
) -> (NetSessionRepo, SessionEventLog, uuid::Uuid) {
    let defs = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(pool, email).await;
    let token = format!("tok-{email}");
    let definition = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            &token,
            now_millis(),
        )
        .await
        .expect("create definition");
    let sessions = NetSessionRepo::new(pool.clone());
    let log = SessionEventLog::new(pool.clone());
    let row = match sessions
        .start(&definition, Some(actor), now_millis())
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    (sessions, log, row.id)
}

#[tokio::test]
async fn start_records_the_starter_as_active_ncs_and_defaults_to_active() {
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let (sessions, _log, session) = start_live_session(&pool, "d2-active@example.com", actor).await;

    let row = sessions.find(session).await.expect("find").expect("exists");
    // The starter is the initial active NCS; control_state defaults active.
    assert_eq!(row.active_ncs_account_id, Some(actor));
    assert_eq!(row.control_state, ControlState::Active);
    assert_eq!(row.stalled_at_millis, None);
}

#[tokio::test]
async fn stall_flips_control_state_and_appends_exactly_one_ncs_stalled_idempotently() {
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let (sessions, log, session) = start_live_session(&pool, "stall@example.com", actor).await;
    let stalled_at = now_millis() + 90_000;

    let event = match sessions.stall(session, stalled_at).await.expect("stall") {
        StallOutcome::Stalled(event) => event,
        other => panic!("expected Stalled, got {other:?}"),
    };
    assert!(matches!(event.body, SessionEventBody::NcsStalled));
    assert_eq!(event.actor_id, None, "stall is system-originated");

    let row = sessions.find(session).await.expect("find").expect("exists");
    assert_eq!(row.control_state, ControlState::Stalled);
    assert_eq!(row.stalled_at_millis, Some(stalled_at));
    // Lifecycle is UNTOUCHED — a stalled session is still Live.
    assert_eq!(row.lifecycle, SessionLifecycle::Live);

    // A second stall tick is idempotent: no second event, still one ncs.stalled.
    assert!(matches!(
        sessions
            .stall(session, stalled_at + 30_000)
            .await
            .expect("stall again"),
        StallOutcome::NotApplicable
    ));
    let events = log.events_since(session, 0).await.expect("read log");
    let stall_count = events
        .iter()
        .filter(|e| matches!(e.body, SessionEventBody::NcsStalled))
        .count();
    assert_eq!(stall_count, 1, "exactly one ncs.stalled despite two ticks");
}

#[tokio::test]
async fn resume_returns_a_stalled_session_to_active_under_the_same_ncs() {
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let (sessions, _log, session) = start_live_session(&pool, "resume@example.com", actor).await;
    sessions
        .stall(session, now_millis() + 90_000)
        .await
        .expect("stall");

    let event = match sessions
        .resume(session, now_millis() + 100_000)
        .await
        .expect("resume")
    {
        ResumeOutcome::Resumed(event) => event,
        other => panic!("expected Resumed, got {other:?}"),
    };
    assert!(matches!(event.body, SessionEventBody::NcsResumed));

    let row = sessions.find(session).await.expect("find").expect("exists");
    assert_eq!(row.control_state, ControlState::Active);
    assert_eq!(row.stalled_at_millis, None);
    assert_eq!(
        row.active_ncs_account_id,
        Some(actor),
        "resume keeps the original NCS"
    );

    // Resume on an already-active session is a no-op.
    assert!(matches!(
        sessions
            .resume(session, now_millis())
            .await
            .expect("resume again"),
        ResumeOutcome::NotApplicable
    ));
}

#[tokio::test]
async fn claim_control_transfers_a_stalled_session_and_refuses_on_a_non_stalled_one() {
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let claimer = seed_account(&pool, "claimer@example.com").await;
    let (sessions, _log, session) = start_live_session(&pool, "claim@example.com", actor).await;

    // A claim on a NON-stalled session is refused with no event (409 later).
    assert!(matches!(
        sessions
            .claim_control(session, claimer, now_millis())
            .await
            .expect("claim active"),
        ClaimControlOutcome::NotStalled
    ));

    sessions
        .stall(session, now_millis() + 90_000)
        .await
        .expect("stall");
    let event = match sessions
        .claim_control(session, claimer, now_millis() + 95_000)
        .await
        .expect("claim stalled")
    {
        ClaimControlOutcome::Claimed(event) => event,
        other => panic!("expected Claimed, got {other:?}"),
    };
    assert!(matches!(
        event.body,
        SessionEventBody::ControlHandedOff { new_ncs_account_id } if new_ncs_account_id == claimer
    ));
    assert_eq!(event.actor_id, Some(claimer));

    let row = sessions.find(session).await.expect("find").expect("exists");
    assert_eq!(
        row.active_ncs_account_id,
        Some(claimer),
        "claimer is the new active NCS"
    );
    assert_eq!(
        row.control_state,
        ControlState::Active,
        "the net resumes under the claimer"
    );
}

#[tokio::test]
async fn hand_off_moves_control_only_for_the_current_active_ncs_on_a_healthy_session() {
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let target = seed_account(&pool, "handoff-target@example.com").await;
    let interloper = uuid::Uuid::now_v7();
    let (sessions, _log, session) = start_live_session(&pool, "handoff@example.com", actor).await;

    // A non-active-NCS caller cannot hand off (the guard matches zero rows).
    assert!(matches!(
        sessions
            .hand_off(session, interloper, target, now_millis())
            .await
            .expect("handoff by other"),
        HandoffOutcome::NotApplicable
    ));

    let event = match sessions
        .hand_off(session, actor, target, now_millis() + 1_000)
        .await
        .expect("handoff by active ncs")
    {
        HandoffOutcome::HandedOff(event) => event,
        other => panic!("expected HandedOff, got {other:?}"),
    };
    assert!(matches!(
        event.body,
        SessionEventBody::ControlHandedOff { new_ncs_account_id } if new_ncs_account_id == target
    ));
    assert_eq!(
        event.actor_id,
        Some(actor),
        "the handing-off NCS is the actor"
    );

    let row = sessions.find(session).await.expect("find").expect("exists");
    assert_eq!(row.active_ncs_account_id, Some(target));
    // Voluntary handoff keeps the net Active — stream uninterrupted.
    assert_eq!(row.control_state, ControlState::Active);
}

#[tokio::test]
async fn a_stalled_session_freezes_every_roster_and_frequency_mutation() {
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let (sessions, log, session) = start_live_session(&pool, "freeze@example.com", actor).await;
    sessions
        .stall(session, now_millis() + 90_000)
        .await
        .expect("stall");
    let seq_before = log.events_since(session, 0).await.expect("read log").len();

    // A check-in on a stalled session is refused (NotLive outcome — the guard's
    // AND control_state='active' matched zero rows) with NO event appended.
    let callsign = parse_callsign("W1AW").expect("valid");
    let add = sessions
        .add_check_in(
            session,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            now_millis() + 91_000,
        )
        .await
        .expect("add attempt");
    assert!(
        matches!(add, AddCheckInOutcome::NotLive),
        "roster frozen while stalled"
    );

    // A frequency change is likewise frozen.
    let freq = sessions
        .change_frequency(
            session,
            uuid::Uuid::now_v7(),
            7_200_000,
            Some(actor),
            now_millis() + 92_000,
        )
        .await
        .expect("freq attempt");
    assert!(
        matches!(freq, ChangeFrequencyOutcome::NotLive),
        "frequency frozen while stalled"
    );

    let seq_after = log.events_since(session, 0).await.expect("read log").len();
    assert_eq!(
        seq_before, seq_after,
        "no phantom events minted while frozen"
    );

    // After a resume the roster is writable again — the freeze was control-state
    // scoped, not a lifecycle close.
    sessions
        .resume(session, now_millis() + 100_000)
        .await
        .expect("resume");
    let add_after = sessions
        .add_check_in(
            session,
            &callsign,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(actor),
            now_millis() + 101_000,
        )
        .await
        .expect("add after resume");
    assert!(
        matches!(add_after, AddCheckInOutcome::Added(_)),
        "writable again after resume"
    );
}

#[tokio::test]
async fn list_live_control_states_reports_the_denormalized_snapshot_for_the_sweep() {
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let (sessions, _log, live) = start_live_session(&pool, "sweep-live@example.com", actor).await;
    let stall_actor = seed_account(&pool, "sweep-stall-actor@example.com").await;
    let (sessions2, _log2, stalled) =
        start_live_session(&pool, "sweep-stalled@example.com", stall_actor).await;
    let stalled_at = now_millis() + 90_000;
    sessions2.stall(stalled, stalled_at).await.expect("stall");

    let mut states = sessions
        .list_live_control_states()
        .await
        .expect("list live control");
    states.sort_by_key(|s| s.session_id);
    let live_snap = states
        .iter()
        .find(|s| s.session_id == live)
        .expect("live present");
    let stalled_snap = states
        .iter()
        .find(|s| s.session_id == stalled)
        .expect("stalled present");

    assert_eq!(live_snap.control_state, ControlState::Active);
    assert_eq!(live_snap.active_ncs_account_id, Some(actor));
    assert_eq!(live_snap.stalled_at_millis, None);

    assert_eq!(stalled_snap.control_state, ControlState::Stalled);
    assert_eq!(stalled_snap.stalled_at_millis, Some(stalled_at));
}

#[tokio::test]
async fn auto_close_reuses_the_shipped_close_guard_and_is_idempotent_from_stalled() {
    let (_container, pool) = migrated_pool().await;
    let actor = seed_account(&pool, "op-actor@example.com").await;
    let (sessions, log, session) = start_live_session(&pool, "autoclose@example.com", actor).await;
    sessions
        .stall(session, now_millis() + 90_000)
        .await
        .expect("stall");

    // Auto-close is the shipped close write with actor_id=None (system-originated),
    // exempt from the control-state freeze: it closes a stalled session.
    let closed_at = now_millis() + 900_000;
    assert!(matches!(
        sessions
            .close(session, closed_at, None)
            .await
            .expect("auto close"),
        CloseOutcome::Closed(_)
    ));
    let row = sessions.find(session).await.expect("find").expect("exists");
    assert_eq!(row.lifecycle, SessionLifecycle::Closed);

    // A second close is idempotent (NotLive) — no second session.closed.
    assert!(matches!(
        sessions
            .close(session, closed_at + 1, None)
            .await
            .expect("second close"),
        CloseOutcome::NotLive
    ));
    let closes = log
        .events_since(session, 0)
        .await
        .expect("read log")
        .iter()
        .filter(|e| matches!(e.body, SessionEventBody::SessionClosed))
        .count();
    assert_eq!(closes, 1, "exactly one session.closed");
}

#[tokio::test]
async fn owned_active_definitions_returns_only_the_accounts_active_owned_nets() {
    // The export's "nets I own" section lists exactly
    // the account's ACTIVE owned nets — archived nets and nets owned by someone
    // else never appear. Returns (definition_id, title) pairs.
    let (_container, pool) = migrated_pool().await;
    let repo = NetDefinitionRepo::new(pool.clone());
    let owner = seed_account(&pool, "owner-export@example.com").await;
    let stranger = seed_account(&pool, "stranger-export@example.com").await;
    let now = now_millis();

    // Two active nets the account owns (distinct titles to assert on).
    let mut active_a_fields = sample_fields();
    active_a_fields.title = "Active Alpha Net".to_owned();
    let active_a = repo
        .create(
            &active_a_fields,
            &sample_connections(),
            owner,
            "tok-own-active-a",
            now,
        )
        .await
        .expect("create active a");

    let mut active_b_fields = sample_fields();
    active_b_fields.title = "Active Bravo Net".to_owned();
    let active_b = repo
        .create(
            &active_b_fields,
            &sample_connections(),
            owner,
            "tok-own-active-b",
            now,
        )
        .await
        .expect("create active b");

    // One archived net the account owns — must be excluded.
    let archived = repo
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            "tok-own-archived",
            now,
        )
        .await
        .expect("create archived");
    assert!(repo.archive(archived.id, now).await.expect("archive"));

    // One active net owned by a DIFFERENT account — must be excluded.
    repo.create(
        &sample_fields(),
        &sample_connections(),
        stranger,
        "tok-stranger",
        now,
    )
    .await
    .expect("create stranger net");

    let mut owned = repo
        .owned_active_definitions(owner)
        .await
        .expect("owned active definitions");
    owned.sort_by(|a, b| a.1.cmp(&b.1));

    assert_eq!(
        owned,
        vec![
            (active_a.id, "Active Alpha Net".to_owned()),
            (active_b.id, "Active Bravo Net".to_owned()),
        ],
        "exactly the two active owned nets, excluding archived and stranger-owned"
    );
}

#[tokio::test]
async fn self_check_ins_returns_only_the_accounts_own_self_check_ins() {
    // The export's "check-in history" means
    // SELF check-ins only. Three checkin.added events:
    // 1. actor = account, source = self -> MUST appear (own check-in).
    // 2. actor = account, source = staff -> MUST NOT appear (account acting as
    // NCS/logger checked in a DIFFERENT station; `actor` is the operator,
    // not the checked-in person — filtering on actor alone would leak this).
    // 3. actor = a different account -> MUST NOT appear.
    let (_container, pool) = migrated_pool().await;
    let defs = NetDefinitionRepo::new(pool.clone());
    let sessions = NetSessionRepo::new(pool.clone());
    let log = SessionEventLog::new(pool.clone());

    let account = seed_account(&pool, "self-checkin@example.com").await;
    let other = seed_account(&pool, "other-op@example.com").await;

    // Session A (owned by the account) — the account's own self check-in lands here.
    let def_a = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            account,
            "tok-self-a",
            now_millis(),
        )
        .await
        .expect("create def a");
    let session_a = match sessions
        .start(&def_a, Some(account), now_millis())
        .await
        .expect("start a")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    // Session B — the staff-entered and stranger check-ins land here.
    let def_b = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            account,
            "tok-self-b",
            now_millis(),
        )
        .await
        .expect("create def b");
    let session_b = match sessions
        .start(&def_b, Some(account), now_millis())
        .await
        .expect("start b")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };

    let own_call = parse_callsign("N1CCK").expect("valid");
    let report = parse_signal_report("599").expect("valid").expect("some");
    let name = parse_name("Nick").expect("valid").expect("some");
    let location = parse_location("Hartford, CT")
        .expect("valid")
        .expect("some");

    // 1. The account's OWN self check-in.
    let self_at = now_millis();
    let outcome = sessions
        .add_check_in(
            session_a.id,
            &own_call,
            uuid::Uuid::now_v7(),
            None,
            Some(&report),
            StayingStatus::StayingForComments,
            Some(&name),
            Some(&location),
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(account),
            self_at,
        )
        .await
        .expect("self check-in");
    assert!(
        matches!(outcome, AddCheckInOutcome::Added(_)),
        "self add succeeds"
    );

    // 2. Account acting as STAFF, logging a DIFFERENT callsign.
    let other_call = parse_callsign("W1AW").expect("valid");
    sessions
        .add_check_in(
            session_b.id,
            &other_call,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(account),
            now_millis(),
        )
        .await
        .expect("staff check-in");

    // 3. A DIFFERENT account's self check-in (never the account's own history).
    let stranger_call = parse_callsign("K2ABC").expect("valid");
    sessions
        .add_check_in(
            session_b.id,
            &stranger_call,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(other),
            now_millis(),
        )
        .await
        .expect("stranger self check-in");

    let history = log.self_check_ins(account).await.expect("self check-ins");
    assert_eq!(
        history.len(),
        1,
        "exactly one self check-in for the account"
    );
    let row = &history[0];
    assert_eq!(row.session_id, session_a.id);
    assert_eq!(row.net_title, "Sunday Traffic Net");
    assert_eq!(row.callsign.as_str(), "N1CCK");
    assert_eq!(row.signal_report.as_ref().map(|r| r.as_str()), Some("599"));
    assert_eq!(row.staying, StayingStatus::StayingForComments);
    assert_eq!(row.name.as_ref().map(|n| n.as_str()), Some("Nick"));
    assert_eq!(
        row.location.as_ref().map(|l| l.as_str()),
        Some("Hartford, CT")
    );
    assert_eq!(row.checked_in_at_millis, self_at);
}

// --- The keyset-paginated profile check-in history --------------

/// The `checkin.added` event ids of `session`, in append order — the durable
/// row ids `self_check_ins_page` pages over. The append API returns the domain
/// `SessionEvent` (seq/actor/at/body), never the storage row id, so a test that
/// wants to assert on ids has to read them back.
async fn checkin_event_ids(pool: &PgPool, session: uuid::Uuid) -> Vec<uuid::Uuid> {
    sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM session_events
         WHERE session_id = $1 AND kind = 'checkin.added'
         ORDER BY seq",
    )
    .bind(session)
    .fetch_all(pool)
    .await
    .expect("read checkin event ids")
}

/// Appends a `checkin.added` row STRAIGHT through the pool, bypassing the
/// domain writer — the only way to author a payload the current writer can no
/// longer produce (an older document with no `source` key) or to seed a page's
/// worth of rows cheaply.
async fn insert_raw_checkin(
    pool: &PgPool,
    session: uuid::Uuid,
    seq: i64,
    actor: uuid::Uuid,
    payload: serde_json::Value,
    at_millis: u64,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, actor, created_at)
         VALUES ($1, $2, $3, 'checkin.added', $4, $5, to_timestamp($6::double precision / 1000.0))",
    )
    .bind(id)
    .bind(session)
    .bind(seq)
    .bind(payload)
    .bind(actor)
    .bind(at_millis as i64)
    .execute(pool)
    .await
    .expect("insert raw checkin event");
    id
}

/// Starts a live session on a fresh net definition owned by `owner`.
async fn seed_live_session(
    pool: &PgPool,
    owner: uuid::Uuid,
    token: &str,
) -> netroll_adapters::pg::net_sessions::NetSessionRow {
    let defs = NetDefinitionRepo::new(pool.clone());
    let sessions = NetSessionRepo::new(pool.clone());
    let def = defs
        .create(
            &sample_fields(),
            &sample_connections(),
            owner,
            token,
            now_millis(),
        )
        .await
        .expect("create def");
    match sessions
        .start(&def, Some(owner), now_millis())
        .await
        .expect("start session")
    {
        StartOutcome::Started(row) => *row,
        other => panic!("expected Started, got {other:?}"),
    }
}

/// Self-checks `account` in under `callsign`, at exactly `at_millis`.
async fn self_check_in_at(
    pool: &PgPool,
    session: uuid::Uuid,
    account: uuid::Uuid,
    callsign: &str,
    at_millis: u64,
) {
    let sessions = NetSessionRepo::new(pool.clone());
    let call = parse_callsign(callsign).expect("valid callsign");
    let outcome = sessions
        .add_check_in(
            session,
            &call,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(account),
            at_millis,
        )
        .await
        .expect("self check-in");
    assert!(matches!(outcome, AddCheckInOutcome::Added(_)));
}

#[tokio::test]
async fn self_check_ins_page_returns_only_the_callers_own_rows() {
    // THE PRIVACY TEST. TWO accounts self-check into the SAME
    // session, interleaved, and account A also STAFF-enters account B's callsign.
    // A one-account version of this test passes against a query with no `actor`
    // predicate at all, which is exactly the bug this forbids.
    let (_container, pool) = migrated_pool().await;
    let log = SessionEventLog::new(pool.clone());
    let sessions = NetSessionRepo::new(pool.clone());

    let a = seed_account(&pool, "history-a@example.com").await;
    let b = seed_account(&pool, "history-b@example.com").await;
    let session = seed_live_session(&pool, a, "tok-hist-shared").await;

    let base = now_millis();
    // 0: A's own self check-in.
    self_check_in_at(&pool, session.id, a, "N1CCK", base + 10).await;
    // 1: B's own self check-in, in the SAME session.
    self_check_in_at(&pool, session.id, b, "K2ABC", base + 20).await;
    // 2: A, acting as staff, logs B's callsign from the radio. `actor` is A, but
    // this is NOT A's check-in — and it is not B's either (no account link).
    let bs_call = parse_callsign("K2ABC").expect("valid");
    sessions
        .add_check_in(
            session.id,
            &bs_call,
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::Staff,
            None,
            None,
            Some(a),
            base + 30,
        )
        .await
        .expect("staff check-in");
    // 3: A's second self check-in.
    self_check_in_at(&pool, session.id, a, "N1CCK", base + 40).await;

    let ids = checkin_event_ids(&pool, session.id).await;
    let (a_first, b_own, a_staff_of_b, a_second) = (ids[0], ids[1], ids[2], ids[3]);

    let a_page = log
        .self_check_ins_page(a, 50, None)
        .await
        .expect("A's history");
    let a_ids: Vec<uuid::Uuid> = a_page.rows.iter().map(|r| r.id).collect();
    assert_eq!(
        a_ids,
        vec![a_second, a_first],
        "A sees exactly its own two self check-ins, newest first"
    );
    assert!(
        !a_ids.contains(&b_own),
        "another account's self check-in must never appear"
    );
    assert!(
        !a_ids.contains(&a_staff_of_b),
        "a staff-entered add A authored is not A's own check-in history"
    );

    let b_page = log
        .self_check_ins_page(b, 50, None)
        .await
        .expect("B's history");
    let b_ids: Vec<uuid::Uuid> = b_page.rows.iter().map(|r| r.id).collect();
    assert_eq!(b_ids, vec![b_own], "B sees exactly its own self check-in");
    assert!(
        !b_ids.contains(&a_staff_of_b),
        "a staff-entered add of B's callsign is not B's history (no account link)"
    );

    // The mirror holds under a limit=1 walk too: a leak that only shows on page
    // two is exactly what a single-page assertion misses.
    let mut walked = Vec::new();
    let mut cursor = None;
    loop {
        let page = log
            .self_check_ins_page(a, 1, cursor)
            .await
            .expect("A's paged history");
        walked.extend(page.rows.iter().map(|r| r.id));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(walked, vec![a_second, a_first]);
}

#[tokio::test]
async fn self_check_ins_page_walks_every_row_exactly_once_newest_first() {
    // The keyset walk is total. Five self check-ins, TWO of
    // which share an identical `created_at` — and the fixture is built so that
    // shared pair straddles the limit=2 page edge, which the walk asserts
    // directly rather than assuming. A `created_at`-only cursor skips or repeats
    // a row there; `(created_at, id)` does not.
    let (_container, pool) = migrated_pool().await;
    let log = SessionEventLog::new(pool.clone());

    let account = seed_account(&pool, "walker@example.com").await;
    let session_a = seed_live_session(&pool, account, "tok-walk-a").await;
    let session_b = seed_live_session(&pool, account, "tok-walk-b").await;

    let base = now_millis();
    // Appended oldest-first; the two `base + 30` rows are the tie.
    self_check_in_at(&pool, session_a.id, account, "N1CCK", base + 10).await;
    self_check_in_at(&pool, session_b.id, account, "N1CCK", base + 20).await;
    self_check_in_at(&pool, session_a.id, account, "N1CCK", base + 30).await;
    self_check_in_at(&pool, session_b.id, account, "N1CCK", base + 30).await;
    self_check_in_at(&pool, session_a.id, account, "N1CCK", base + 40).await;

    let seeded: std::collections::HashSet<uuid::Uuid> = checkin_event_ids(&pool, session_a.id)
        .await
        .into_iter()
        .chain(checkin_event_ids(&pool, session_b.id).await)
        .collect();
    assert_eq!(seeded.len(), 5);

    let first = log
        .self_check_ins_page(account, 2, None)
        .await
        .expect("page 1");
    assert_eq!(first.rows.len(), 2);
    let resume = first.next.expect("a second page exists");
    let second = log
        .self_check_ins_page(account, 2, Some(resume))
        .await
        .expect("page 2");
    assert_eq!(second.rows.len(), 2);
    // THE FIXTURE'S OWN PRECONDITION: the tie really does straddle this edge.
    assert_eq!(
        first.rows[1].checked_in_at_millis, second.rows[0].checked_in_at_millis,
        "page 1's last row and page 2's first row must share a created_at, \
         or this test proves nothing about a shared-timestamp boundary"
    );

    // Walk to exhaustion from the top and assert totality + strict ordering.
    let mut walked: Vec<(u64, uuid::Uuid)> = Vec::new();
    let mut cursor = None;
    loop {
        let page = log
            .self_check_ins_page(account, 2, cursor)
            .await
            .expect("walk page");
        walked.extend(page.rows.iter().map(|r| (r.checked_in_at_millis, r.id)));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    let walked_ids: std::collections::HashSet<uuid::Uuid> =
        walked.iter().map(|(_, id)| *id).collect();
    assert_eq!(walked.len(), 5, "no duplicates and no gaps");
    assert_eq!(walked_ids, seeded, "exactly the seeded rows");
    for pair in walked.windows(2) {
        assert!(
            pair[0] > pair[1],
            "strictly newest-first by (created_at, id): {pair:?}"
        );
    }
}

#[tokio::test]
async fn a_pre_4_9_payload_with_no_source_key_is_not_returned() {
    // THE SQL/RUST AGREEMENT TEST. The `source` discard moved
    // from Rust into SQL, and the two must answer identically for a HISTORICAL
    // payload with no `source` key: in Rust an absent key folds to
    // `CheckInSource::Staff` (excluded); in SQL `payload->>'source'` is NULL and
    // `NULL = 'self'` is not true (excluded). If `default_source_token` ever
    // changed, this test is what catches the divergence.
    let (_container, pool) = migrated_pool().await;
    let log = SessionEventLog::new(pool.clone());

    let account = seed_account(&pool, "historical@example.com").await;
    let session = seed_live_session(&pool, account, "tok-historical").await;

    let sourceless = insert_raw_checkin(
        &pool,
        session.id,
        900,
        account,
        serde_json::json!({
            "checkInId": uuid::Uuid::now_v7().to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
        }),
        now_millis(),
    )
    .await;

    let page = log
        .self_check_ins_page(account, 50, None)
        .await
        .expect("paged history");
    assert!(
        !page.rows.iter().any(|r| r.id == sourceless),
        "a payload with no `source` key is not a self check-in"
    );
    assert!(page.rows.is_empty(), "it was the only seeded row");

    // unbounded export read reaches the same verdict on the same row.
    let exported = log.self_check_ins(account).await.expect("export history");
    assert!(
        exported.is_empty(),
        "the SQL predicate and the Rust discard must agree on this row"
    );
}

#[tokio::test]
async fn self_check_ins_page_clamps_an_oversized_limit() {
    // A caller asking for the world gets MAX_PAGE_LIMIT and a
    // cursor, never the whole history in one response.
    let (_container, pool) = migrated_pool().await;
    let log = SessionEventLog::new(pool.clone());

    let account = seed_account(&pool, "oversized@example.com").await;
    let session = seed_live_session(&pool, account, "tok-oversized").await;

    let base = now_millis();
    for i in 0..(MAX_PAGE_LIMIT + 1) {
        insert_raw_checkin(
            &pool,
            session.id,
            1_000 + i as i64,
            account,
            serde_json::json!({
                "checkInId": uuid::Uuid::now_v7().to_string(),
                "callsign": "W1AW",
                "staying": "in-and-out",
                "source": "self",
            }),
            base + i as u64,
        )
        .await;
    }

    let page = log
        .self_check_ins_page(account, clamp_limit(Some(99_999)), None)
        .await
        .expect("clamped page");
    assert_eq!(page.rows.len(), MAX_PAGE_LIMIT);
    assert!(page.next.is_some(), "a further page is offered");
}

// --- Durable on-close delivery jobs ------------------------------
//
// The enqueue lives INSIDE `NetSessionRepo::close`'s transaction, so these tests
// reach it through `close()` and read the table back with plain SQL. The claim
// tests seed rows directly: they are about the claim's concurrency guarantee,
// not about planning.

/// Starts a live session for a fresh definition and returns the repo handles
/// plus the session id — the arrange step every job test shares.
async fn seed_live_session_for_jobs(
    pool: &PgPool,
    email: &str,
) -> (
    NetSessionRepo,
    netroll_domain::net::NetDefinition,
    uuid::Uuid,
) {
    let (sessions, _log, definition) = seed_definition(pool, email).await;
    let row = match sessions
        .start(&definition, None, now_millis())
        .await
        .expect("atomic start")
    {
        StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    (sessions, definition, row.id)
}

fn delivery_fields(
    emails: &[&str],
    webhook: Option<&str>,
    discord: Option<&str>,
) -> DeliveryConfigFields {
    DeliveryConfigFields {
        emails: emails.iter().map(|e| (*e).to_owned()).collect(),
        webhook_url: webhook.map(str::to_owned),
        discord_webhook_url: discord.map(str::to_owned),
    }
}

/// Every leg of one session as `(destination, target, state, attempts)`, in a
/// stable order, straight off the table.
async fn legs_of(pool: &PgPool, session_id: uuid::Uuid) -> Vec<(String, String, String, i32)> {
    sqlx::query_as::<_, (String, String, String, i32)>(
        "SELECT destination, target, state, attempts FROM net_delivery_jobs
          WHERE session_id = $1 ORDER BY destination, target",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .expect("read legs")
}

fn instant(millis: u64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(millis as i64).expect("test instant in chrono range")
}

/// Seeds one leg row with explicit scheduling columns — the claim/prune tests'
/// fixture, bypassing the planner on purpose.
#[allow(clippy::too_many_arguments)]
async fn insert_leg(
    pool: &PgPool,
    session_id: uuid::Uuid,
    destination: &str,
    target: &str,
    state: &str,
    attempts: i32,
    next_attempt_at: u64,
    claimed_until: Option<u64>,
    completed_at: Option<u64>,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO net_delivery_jobs
             (id, session_id, destination, target, state, attempts, next_attempt_at,
              claimed_until, created_at, completed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $7, $9)",
    )
    .bind(id)
    .bind(session_id)
    .bind(destination)
    .bind(target)
    .bind(state)
    .bind(attempts)
    .bind(instant(next_attempt_at))
    .bind(claimed_until.map(instant))
    .bind(completed_at.map(instant))
    .execute(pool)
    .await
    .expect("seed leg");
    id
}

const LEASE_MILLIS: u64 = 180_000;

#[tokio::test]
async fn closing_a_session_plans_one_pending_leg_per_armed_destination() {
    // The debt is recorded in the close's own transaction —
    // two addresses, a webhook and a Discord URL are four legs, each `pending`
    // with zero attempts, before any delivery task has run at all.
    let (_container, pool) = migrated_pool().await;
    let (sessions, definition, session_id) =
        seed_live_session_for_jobs(&pool, "plan@example.com").await;
    DeliveryConfigRepo::new(pool.clone())
        .set(
            definition.id,
            &delivery_fields(
                &["log@example.com", "alerts@example.org"],
                Some("https://hooks.example.com/net"),
                Some("https://discord.com/api/webhooks/1/tok"),
            ),
            Some("minted-secret"),
            now_millis(),
        )
        .await
        .expect("set config");

    let outcome = sessions
        .close(session_id, now_millis(), None)
        .await
        .expect("close");
    assert!(matches!(outcome, CloseOutcome::Closed(_)));

    let legs = legs_of(&pool, session_id).await;
    assert_eq!(
        legs,
        vec![
            ("discord".to_owned(), String::new(), "pending".to_owned(), 0),
            (
                "email".to_owned(),
                "alerts@example.org".to_owned(),
                "pending".to_owned(),
                0
            ),
            (
                "email".to_owned(),
                "log@example.com".to_owned(),
                "pending".to_owned(),
                0
            ),
            ("webhook".to_owned(), String::new(), "pending".to_owned(), 0),
        ],
        "one pending leg per armed destination, planned by the close itself"
    );
}

#[tokio::test]
async fn a_net_with_no_config_row_plans_no_legs() {
    // "Zero legs planned", first half: no config
    // row at all. (Two tests rather than one because `seed_definition` mints a
    // fixed link token, so a second definition in one database collides.)
    let (_container, pool) = migrated_pool().await;
    let (sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "off-a@example.com").await;
    sessions
        .close(session_id, now_millis(), None)
        .await
        .expect("close");
    assert!(
        legs_of(&pool, session_id).await.is_empty(),
        "no config row → no legs"
    );
}

#[tokio::test]
async fn a_config_row_arming_nothing_plans_no_legs() {
    // "Zero legs planned", second half: a row exists but arms no
    // destination — "delivery off" must not become "a row exists".
    let (_container, pool) = migrated_pool().await;
    let (sessions, definition, session_id) =
        seed_live_session_for_jobs(&pool, "off-b@example.com").await;
    DeliveryConfigRepo::new(pool.clone())
        .set(
            definition.id,
            &delivery_fields(&[], None, None),
            None,
            now_millis(),
        )
        .await
        .expect("set empty config");
    sessions
        .close(session_id, now_millis(), None)
        .await
        .expect("close");
    assert!(
        legs_of(&pool, session_id).await.is_empty(),
        "a config row arming nothing → no legs"
    );
}

#[tokio::test]
async fn a_losing_close_plans_no_legs() {
    // The rollback path must not plan. Close once (legs planned), wipe them,
    // close again: the second close is `NotLive` and must leave the table empty.
    let (_container, pool) = migrated_pool().await;
    let (sessions, definition, session_id) =
        seed_live_session_for_jobs(&pool, "lose@example.com").await;
    DeliveryConfigRepo::new(pool.clone())
        .set(
            definition.id,
            &delivery_fields(&["log@example.com"], None, None),
            None,
            now_millis(),
        )
        .await
        .expect("set config");
    sessions
        .close(session_id, now_millis(), None)
        .await
        .expect("first close");
    assert_eq!(legs_of(&pool, session_id).await.len(), 1);
    sqlx::query("DELETE FROM net_delivery_jobs WHERE session_id = $1")
        .bind(session_id)
        .execute(&pool)
        .await
        .expect("wipe legs");

    let second = sessions
        .close(session_id, now_millis() + 1, None)
        .await
        .expect("second close");
    assert!(matches!(second, CloseOutcome::NotLive));
    assert!(
        legs_of(&pool, session_id).await.is_empty(),
        "a NotLive close rolled back and planned nothing"
    );
    let missing = sessions
        .close(uuid::Uuid::now_v7(), now_millis(), None)
        .await
        .expect("missing close");
    assert!(matches!(missing, CloseOutcome::Missing));
}

#[tokio::test]
async fn a_state_this_build_cannot_name_is_refused_at_the_boundary() {
    // `state` was a bare `text` with the vocabulary only in a comment. Every
    // query in the repo is written against one of the four known values:
    // `due_sessions`, `claim_session`, `interrupted`, `claimed`, `mark_terminal`,
    // `reschedule` and `skip_session` all key on `state = 'pending'`, and
    // `prune_terminal` on `state <> 'pending' AND completed_at IS NOT NULL`. A
    // fifth value written by a newer deploy and then rolled back therefore
    // matches NOTHING: never claimed, never recovered, never pruned, never
    // reported — a permanent invisible delivery debt. A CHECK turns that into
    // an error where it is written instead.
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "unknown@example.com").await;
    let now = now_millis();

    let refused = sqlx::query(
        "INSERT INTO net_delivery_jobs (id, session_id, destination, target, state,
                                        attempts, next_attempt_at, created_at)
         VALUES ($1, $2, 'email', 'x@example.com', 'deferred', 0, $3, $3)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(session_id)
    .bind(instant(now))
    .execute(&pool)
    .await;
    assert!(
        refused.is_err(),
        "a state no arm matches must not be storable"
    );

    // And the four the build does name are all still storable.
    for state in ["pending", "succeeded", "failed", "skipped"] {
        sqlx::query(
            "INSERT INTO net_delivery_jobs (id, session_id, destination, target, state,
                                            attempts, next_attempt_at, created_at)
             VALUES ($1, $2, 'email', $3, $4, 0, $5, $5)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(session_id)
        .bind(format!("{state}@example.com"))
        .bind(state)
        .bind(instant(now))
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("{state} must remain storable: {e}"));
    }
}

#[tokio::test]
async fn a_recovery_read_is_bounded_by_its_limit() {
    // The boot pass is AWAITED before the server serves and writes one row at a
    // time, so an unbounded read after a long outage would gate the process on
    // the size of the backlog. The claim was bounded from the start; these
    // two were not.
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "bounded@example.com").await;
    let now = now_millis();
    for i in 0..5 {
        insert_leg(
            &pool,
            session_id,
            "email",
            &format!("r{i}@example.com"),
            "pending",
            1,
            now,
            Some(now + 10),
            None,
        )
        .await;
    }
    let repo = DeliveryJobRepo::new(pool.clone());
    let later = now + LEASE_MILLIS + 1;

    assert_eq!(
        repo.interrupted(later, 2).await.expect("interrupted").len(),
        2,
        "an expired-lease read stops at its limit"
    );
    assert_eq!(
        repo.claimed(3).await.expect("claimed").len(),
        3,
        "and so does the boot read"
    );
}

#[tokio::test]
async fn a_locked_leg_is_skipped_by_a_claimer_rather_than_waited_on() {
    // `FOR UPDATE SKIP LOCKED`'s actual property, which the two-claimer race
    // test cannot see: a claimer does not WAIT on a row someone else has
    // locked. Without `SKIP LOCKED` this claim blocks until the holding
    // transaction ends, which on a sweep tick means the tick stalls behind
    // whatever is holding the row.
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "locked@example.com").await;
    let now = now_millis();
    insert_leg(
        &pool, session_id, "webhook", "", "pending", 0, now, None, None,
    )
    .await;
    let repo = DeliveryJobRepo::new(pool.clone());

    let mut holder = pool.begin().await.expect("begin");
    sqlx::query("SELECT id FROM net_delivery_jobs WHERE session_id = $1 FOR UPDATE")
        .bind(session_id)
        .fetch_all(&mut *holder)
        .await
        .expect("hold the row lock");

    let claimed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        repo.claim_session(session_id, now, LEASE_MILLIS),
    )
    .await
    .expect("a claimer must SKIP a locked row, never wait on it")
    .expect("claim");
    assert!(claimed.is_empty(), "the locked row is skipped, not claimed");

    holder.rollback().await.expect("release the lock");
    assert_eq!(
        repo.claim_session(session_id, now, LEASE_MILLIS)
            .await
            .expect("claim")
            .len(),
        1,
        "and once released it claims normally"
    );
}

#[tokio::test]
async fn a_settle_from_a_spent_claim_moves_nothing_and_says_so() {
    // The claim is atomic; the SETTLE was not fenced by it. A run whose send
    // outlived its lease would move a row a NEWER claim already owned —
    // releasing a leg that is still being sent, or terminating one another
    // attempt is working on — and, because the write discarded its row count,
    // report the transition as though it had happened.
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "stale@example.com").await;
    let now = now_millis();
    insert_leg(
        &pool, session_id, "webhook", "", "pending", 0, now, None, None,
    )
    .await;
    let repo = DeliveryJobRepo::new(pool.clone());

    let first = repo
        .claim_session(session_id, now, LEASE_MILLIS)
        .await
        .expect("claim");
    assert_eq!(first.len(), 1);
    let stale = first[0].clone();

    // The lease lapses, recovery releases the leg, and a newer claim takes it.
    let later = now + LEASE_MILLIS + 1;
    assert_eq!(
        repo.reschedule(stale.id, later, stale.lease_until_millis)
            .await
            .expect("release"),
        1,
        "the release belongs to the claim that owned the leg"
    );
    let second = repo
        .claim_session(session_id, later, LEASE_MILLIS)
        .await
        .expect("second claim");
    assert_eq!(second.len(), 1);
    assert_ne!(
        second[0].lease_until_millis, stale.lease_until_millis,
        "a new claim stamps a new lease"
    );

    // The first run's send finally returns and settles against ITS claim.
    assert_eq!(
        repo.mark_terminal(
            stale.id,
            TerminalState::Succeeded,
            later,
            stale.lease_until_millis
        )
        .await
        .expect("stale settle"),
        0,
        "a settle from a spent claim moves no row"
    );
    let legs = legs_of(&pool, session_id).await;
    assert_eq!(
        legs[0].2, "pending",
        "the leg still belongs to the newer claim"
    );
}

#[tokio::test]
async fn a_due_leg_is_claimed_by_exactly_one_of_two_concurrent_claimers() {
    // Exactly one winner, and exactly one attempt burned. Note what this does
    // NOT prove: delete `SKIP LOCKED` and it still passes, because under read
    // committed the loser blocks on the row lock, re-evaluates once the winner
    // commits, sees `claimed_until IS NOT NULL` and returns nothing. The half
    // proved here is the `claimed_until IS NULL` predicate. `SKIP LOCKED`'s own
    // property is NOT BLOCKING, and it is asserted by
    // `a_locked_leg_is_skipped_by_a_claimer_rather_than_waited_on`.
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "race@example.com").await;
    let now = now_millis();
    insert_leg(
        &pool, session_id, "webhook", "", "pending", 0, now, None, None,
    )
    .await;

    let repo = DeliveryJobRepo::new(pool.clone());
    let a = {
        let repo = repo.clone();
        tokio::spawn(async move {
            repo.claim_session(session_id, now, LEASE_MILLIS)
                .await
                .expect("claim a")
        })
    };
    let b = {
        let repo = repo.clone();
        tokio::spawn(async move {
            repo.claim_session(session_id, now, LEASE_MILLIS)
                .await
                .expect("claim b")
        })
    };
    let (claimed_a, claimed_b) = (a.await.expect("join a"), b.await.expect("join b"));

    assert_eq!(
        claimed_a.len() + claimed_b.len(),
        1,
        "exactly one claimer gets the leg: {claimed_a:?} / {claimed_b:?}"
    );
    let winner = claimed_a
        .into_iter()
        .chain(claimed_b)
        .next()
        .expect("one winner");
    assert_eq!(winner.session_id, session_id);
    assert_eq!(winner.attempts, 1, "the claim burned one attempt");
    let legs = legs_of(&pool, session_id).await;
    assert_eq!(legs[0].3, 1, "and the row agrees");
    // A third claim right after finds nothing: the lease is live.
    assert!(
        repo.claim_session(session_id, now, LEASE_MILLIS)
            .await
            .expect("claim c")
            .is_empty()
    );
}

#[tokio::test]
async fn an_unclaimable_leg_is_invisible_to_a_claim_and_an_expired_lease_is_interrupted() {
    // Four legs a claim must NOT return: not yet due, leased and unexpired,
    // already terminal, and leased-but-EXPIRED. The last one is the deliberate
    // asymmetry — an expired lease is surfaced by `interrupted`, never
    // re-claimed blindly, because whether it may be retried is per destination.
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "invis@example.com").await;
    let now = now_millis();
    insert_leg(
        &pool,
        session_id,
        "email",
        "later@example.com",
        "pending",
        0,
        now + 60_000,
        None,
        None,
    )
    .await;
    insert_leg(
        &pool,
        session_id,
        "webhook",
        "",
        "pending",
        1,
        now,
        Some(now + LEASE_MILLIS),
        None,
    )
    .await;
    insert_leg(
        &pool,
        session_id,
        "email",
        "done@example.com",
        "succeeded",
        1,
        now,
        None,
        Some(now),
    )
    .await;
    let expired = insert_leg(
        &pool,
        session_id,
        "discord",
        "",
        "pending",
        1,
        now,
        Some(now - 1),
        None,
    )
    .await;

    let repo = DeliveryJobRepo::new(pool.clone());
    assert!(
        repo.claim_session(session_id, now, LEASE_MILLIS)
            .await
            .expect("claim")
            .is_empty(),
        "none of the four is claimable"
    );
    let interrupted = repo.interrupted(now, 256).await.expect("interrupted");
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].id, expired);
    assert_eq!(interrupted[0].destination, "discord");
}

#[tokio::test]
async fn prune_terminal_is_idempotent_and_never_touches_a_pending_row() {
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "prune@example.com").await;
    let now = now_millis();
    let retention = 30 * 24 * HOUR_MILLIS;
    // Terminal and past the window; terminal and inside it; pending and ancient.
    insert_leg(
        &pool,
        session_id,
        "email",
        "old@example.com",
        "failed",
        10,
        now - retention - 2 * HOUR_MILLIS,
        None,
        Some(now - retention - HOUR_MILLIS),
    )
    .await;
    insert_leg(
        &pool,
        session_id,
        "email",
        "new@example.com",
        "succeeded",
        1,
        now,
        None,
        Some(now - HOUR_MILLIS),
    )
    .await;
    insert_leg(
        &pool,
        session_id,
        "webhook",
        "",
        "pending",
        0,
        now - retention - HOUR_MILLIS,
        None,
        None,
    )
    .await;

    let repo = DeliveryJobRepo::new(pool.clone());
    assert_eq!(repo.prune_terminal(now, retention).await.expect("prune"), 1);
    assert_eq!(
        repo.prune_terminal(now, retention)
            .await
            .expect("prune again"),
        0,
        "idempotent"
    );
    let remaining = legs_of(&pool, session_id).await;
    assert_eq!(remaining.len(), 2);
    assert!(
        remaining
            .iter()
            .any(|(d, _, state, _)| d == "webhook" && state == "pending"),
        "a pending debt is never pruned, however old"
    );
}

#[tokio::test]
async fn skip_session_moves_every_pending_leg_and_leaves_terminal_ones_alone() {
    let (_container, pool) = migrated_pool().await;
    let (_sessions, _definition, session_id) =
        seed_live_session_for_jobs(&pool, "skip@example.com").await;
    let now = now_millis();
    insert_leg(
        &pool,
        session_id,
        "email",
        "a@example.com",
        "pending",
        1,
        now,
        Some(now + LEASE_MILLIS),
        None,
    )
    .await;
    insert_leg(
        &pool, session_id, "webhook", "", "pending", 0, now, None, None,
    )
    .await;
    insert_leg(
        &pool,
        session_id,
        "discord",
        "",
        "succeeded",
        1,
        now,
        None,
        Some(now),
    )
    .await;

    let repo = DeliveryJobRepo::new(pool.clone());
    assert_eq!(repo.skip_session(session_id, now).await.expect("skip"), 2);
    let legs = legs_of(&pool, session_id).await;
    assert_eq!(legs.iter().filter(|(_, _, s, _)| s == "skipped").count(), 2);
    assert_eq!(
        legs.iter().filter(|(_, _, s, _)| s == "succeeded").count(),
        1
    );
    // And a terminal row cannot be re-terminated with a different verdict.
    let succeeded = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM net_delivery_jobs WHERE session_id = $1 AND state = 'succeeded'",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .expect("succeeded id");
    // Any lease value: the `state = 'pending'` half of the guard already refuses
    // a terminal row, and this asserts that half specifically.
    assert_eq!(
        repo.mark_terminal(succeeded, TerminalState::Failed, now, now)
            .await
            .expect("mark"),
        0,
        "a terminal row is not re-terminated, and the write says so"
    );
    assert_eq!(
        legs_of(&pool, session_id)
            .await
            .iter()
            .filter(|(_, _, s, _)| s == "succeeded")
            .count(),
        1
    );
}
