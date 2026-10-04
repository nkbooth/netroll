// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Integration tests against a real containerized Postgres: abuse-report
//! record/list/resolve, the append-only audit log, and account
//! disable/reenable with session revoke. Assert the recorded ROWS and
//! decisions, never message prose.

use netroll_adapters::pg::abuse_reports::{AbuseReportRepo, ResolveOutcome};
use netroll_adapters::pg::accounts::{AccountRepo, DisableOutcome, ReenableOutcome};
use netroll_adapters::pg::audit_log::{AuditEntry, AuditLogRepo};
use netroll_adapters::pg::sessions::SessionRepo;
use netroll_domain::admin::PageCursor;
use sqlx::PgPool;
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

async fn seed_account(pool: &PgPool, email: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, $2)")
        .bind(id)
        .bind(email)
        .execute(pool)
        .await
        .expect("seed account");
    id
}

async fn live_session_count(pool: &PgPool, account_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sessions WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await
    .expect("count live sessions")
}

// ---- abuse reports ------------------------------------------------------------

#[tokio::test]
async fn a_recorded_report_round_trips_and_resolve_clears_it_from_the_queue() {
    let (_c, pool) = migrated_pool().await;
    let reports = AbuseReportRepo::new(pool.clone());
    let admin = seed_account(&pool, "admin@example.com").await;

    let id = reports
        .record(
            "spam net titles",
            Some("W1RPT"),
            Some("https://x/nets"),
            1_000,
        )
        .await
        .expect("record");

    let queue = reports
        .list_unresolved_page(10, None)
        .await
        .expect("page the queue")
        .rows;
    assert_eq!(queue.len(), 1, "the recorded report is in the review queue");
    let stored = &queue[0];
    assert_eq!(stored.id, id);
    assert_eq!(stored.body, "spam net titles");
    assert_eq!(stored.reporter_contact.as_deref(), Some("W1RPT"));
    assert_eq!(stored.context_url.as_deref(), Some("https://x/nets"));
    assert_eq!(stored.created_at_millis, 1_000);
    assert!(stored.resolved_at_millis.is_none());

    // Resolving stamps resolved_at/resolved_by and drops it from the queue.
    assert_eq!(
        reports.resolve(id, admin, 2_000).await.expect("resolve"),
        ResolveOutcome::Resolved
    );
    assert!(
        reports
            .list_unresolved_page(10, None)
            .await
            .expect("page the queue")
            .rows
            .is_empty(),
        "a resolved report leaves the unresolved queue"
    );

    // Re-resolve is idempotent (no second stamp).
    assert_eq!(
        reports.resolve(id, admin, 3_000).await.expect("re-resolve"),
        ResolveOutcome::AlreadyResolved
    );
}

#[tokio::test]
async fn resolving_a_missing_report_is_row_not_found() {
    let (_c, pool) = migrated_pool().await;
    let reports = AbuseReportRepo::new(pool.clone());
    let admin = seed_account(&pool, "admin@example.com").await;
    let missing = reports.resolve(Uuid::now_v7(), admin, 1_000).await;
    assert!(matches!(missing, Err(sqlx::Error::RowNotFound)));
}

#[tokio::test]
async fn a_report_records_without_optional_contact_or_context() {
    let (_c, pool) = migrated_pool().await;
    let reports = AbuseReportRepo::new(pool.clone());
    reports
        .record("anonymous report", None, None, 5_000)
        .await
        .expect("record anonymous");
    let queue = reports
        .list_unresolved_page(10, None)
        .await
        .expect("page the queue")
        .rows;
    assert_eq!(queue.len(), 1);
    assert!(queue[0].reporter_contact.is_none());
    assert!(queue[0].context_url.is_none());
}

// ---- audit log ----------------------------------------------------------------

#[tokio::test]
async fn an_appended_entry_stores_exactly_actor_action_target_and_time() {
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "admin@example.com").await;
    let target = Uuid::now_v7();

    audit
        .append(
            &AuditEntry {
                actor_account_id: actor,
                action: "disable-account".to_owned(),
                target_type: Some("account".to_owned()),
                target_id: Some(target),
                metadata: Some(serde_json::json!({ "outcome": "disabled" })),
                context_session_id: None,
                context_definition_id: None,
            },
            7_000,
        )
        .await
        .expect("append");

    let entries = audit
        .list_page(10, None)
        .await
        .expect("page the audit log")
        .rows;
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.actor_account_id, actor);
    assert_eq!(e.action, "disable-account");
    assert_eq!(e.target_type.as_deref(), Some("account"));
    assert_eq!(e.target_id, Some(target));
    assert_eq!(e.occurred_at_millis, 7_000);
    assert_eq!(
        e.metadata,
        Some(serde_json::json!({ "outcome": "disabled" }))
    );
}

#[tokio::test]
async fn since_windows_the_audit_log_by_time() {
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "admin@example.com").await;
    let entry = |action: &str| AuditEntry {
        actor_account_id: actor,
        action: action.to_owned(),
        target_type: None,
        target_id: None,
        metadata: None,
        context_session_id: None,
        context_definition_id: None,
    };
    audit
        .append(&entry("view-reports"), 1_000)
        .await
        .expect("old");
    audit
        .append(&entry("resolve-report"), 9_000)
        .await
        .expect("new");

    let recent = audit
        .since_page(10, None, 5_000)
        .await
        .expect("since_page")
        .rows;
    assert_eq!(recent.len(), 1, "only the entry at/after the cutoff");
    assert_eq!(recent[0].action, "resolve-report");
}

#[tokio::test]
async fn since_page_is_bounded_and_pages_the_whole_window() {
    // The windowed read must be bounded like every other admin read, AND the
    // bound must not cost completeness: walking the cursor has to reach every
    // in-window row exactly once and never reach outside the window.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "admin@example.com").await;

    let mut before: Vec<Uuid> = Vec::new();
    for at in [1_000_u64, 4_999] {
        before.push(append_ctx(&audit, actor, "view-reports", None, None, None, at).await);
    }
    // 7_000 appears twice, and newest-first at limit=2 that pair is SPLIT by the
    // first page boundary: page 1 ends on the later 7_000 row and page 2 must
    // resume at the earlier one. A timestamp-only cursor (`occurred_at < $1`)
    // excludes every 7_000 row there and drops it — which is what makes the `id`
    // tiebreak observable rather than merely claimed. 5_000 sits exactly ON the
    // cutoff (the window is inclusive).
    let mut in_window: Vec<Uuid> = Vec::new();
    for at in [5_000_u64, 6_000, 7_000, 7_000, 8_000] {
        in_window.push(append_ctx(&audit, actor, "view-reports", None, None, None, at).await);
    }
    in_window.reverse(); // the read is newest-first

    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<PageCursor> = None;
    loop {
        let page = audit
            .since_page(2, cursor, 5_000)
            .await
            .expect("page the window");
        assert!(page.rows.len() <= 2, "a page never exceeds its limit");
        seen.extend(page.rows.iter().map(|e| e.id));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(
        seen, in_window,
        "every at-or-after-cutoff row exactly once, newest first, across every boundary"
    );
    for id in before {
        assert!(
            !seen.contains(&id),
            "a pre-cutoff row never enters the window"
        );
    }
}

#[tokio::test]
async fn a_paged_read_serves_at_most_its_limit_and_says_more_remain() {
    // The bound is only honest if the caller can tell, from the returned value
    // alone, that it was truncated. `next` is that signal.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let reports = AbuseReportRepo::new(pool.clone());
    let actor = seed_account(&pool, "admin@example.com").await;

    for at in [1_000_u64, 2_000, 3_000, 4_000] {
        append_ctx(&audit, actor, "view-reports", None, None, None, at).await;
        reports
            .record("report", None, None, at)
            .await
            .expect("record report");
    }

    let audit_page = audit.list_page(3, None).await.expect("page the audit log");
    assert_eq!(audit_page.rows.len(), 3);
    assert!(audit_page.next.is_some(), "the caller can see more remain");

    let window = audit
        .since_page(3, None, 1_000)
        .await
        .expect("page the window");
    assert_eq!(window.rows.len(), 3);
    assert!(window.next.is_some());

    let queue = reports
        .list_unresolved_page(3, None)
        .await
        .expect("page the queue");
    assert_eq!(queue.rows.len(), 3);
    assert!(queue.next.is_some());
}

// ---- account disable / reenable ----------------------------------------------

#[tokio::test]
async fn disabling_revokes_live_sessions_and_blocks_the_account_until_reenabled() {
    let (_c, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let sessions = SessionRepo::new(pool.clone());
    let account = seed_account(&pool, "abuser@example.com").await;

    // Give the account a live session (the "signed in then disabled" case).
    sessions
        .insert(account, [1u8; 32], 1_000, 10_000_000)
        .await
        .expect("insert session");
    assert_eq!(live_session_count(&pool, account).await, 1);
    assert!(!accounts.is_disabled(account).await.expect("is_disabled"));

    // Disable: sets disabled_at AND revokes the live session in one tx.
    assert_eq!(
        accounts
            .disable(account, 2_000, Some("abuse"))
            .await
            .expect("disable"),
        DisableOutcome::Disabled
    );
    assert!(accounts.is_disabled(account).await.expect("is_disabled"));
    assert_eq!(
        live_session_count(&pool, account).await,
        0,
        "disabling revokes the account's live sessions"
    );

    // Re-disable is idempotent.
    assert_eq!(
        accounts
            .disable(account, 3_000, None)
            .await
            .expect("re-disable"),
        DisableOutcome::AlreadyDisabled
    );

    // Only reenable clears it — and it does NOT touch deleted_at.
    assert_eq!(
        accounts.reenable(account, 4_000).await.expect("reenable"),
        ReenableOutcome::Reenabled
    );
    assert!(!accounts.is_disabled(account).await.expect("is_disabled"));
    assert_eq!(
        accounts
            .reenable(account, 5_000)
            .await
            .expect("re-reenable"),
        ReenableOutcome::AlreadyEnabled
    );
}

#[tokio::test]
async fn disable_does_not_set_deleted_at_and_a_reenabled_account_is_not_pending() {
    let (_c, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    let account = seed_account(&pool, "sep@example.com").await;

    accounts
        .disable(account, 1_000, None)
        .await
        .expect("disable");
    let found = accounts
        .find_by_id(account)
        .await
        .expect("find")
        .expect("account exists");
    assert_eq!(found.disabled_at_millis, Some(1_000));
    assert!(
        found.deleted_at_millis.is_none(),
        "disabling must not set deleted_at (distinct states)"
    );
}

#[tokio::test]
async fn disabling_or_reenabling_a_missing_account_is_row_not_found() {
    let (_c, pool) = migrated_pool().await;
    let accounts = AccountRepo::new(pool.clone());
    assert!(matches!(
        accounts.disable(Uuid::now_v7(), 1_000, None).await,
        Err(sqlx::Error::RowNotFound)
    ));
    assert!(matches!(
        accounts.reenable(Uuid::now_v7(), 1_000).await,
        Err(sqlx::Error::RowNotFound)
    ));
}

// ---- keyset pagination on the admin review reads -------------------------------

#[tokio::test]
async fn paging_the_report_queue_walks_every_row_exactly_once() {
    // The queue is oldest-first, so a page boundary that skipped or repeated a
    // row would hide a report from review or make it look like two.
    let (_c, pool) = migrated_pool().await;
    let reports = AbuseReportRepo::new(pool.clone());

    // Two reports share a millisecond: the id tiebreak is what keeps the keyset
    // total. Without it this test is exactly where paging breaks.
    let stamps = [1_000_u64, 2_000, 2_000, 3_000, 4_000];
    let mut expected: Vec<Uuid> = Vec::new();
    for (n, at) in stamps.iter().enumerate() {
        expected.push(
            reports
                .record(&format!("report {n}"), None, None, *at)
                .await
                .expect("record report"),
        );
    }

    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<PageCursor> = None;
    loop {
        let page = reports
            .list_unresolved_page(2, cursor)
            .await
            .expect("page the queue");
        assert!(page.rows.len() <= 2, "a page never exceeds its limit");
        seen.extend(page.rows.iter().map(|r| r.id));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(seen.len(), stamps.len(), "every row is served exactly once");
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "no row is served twice");

    // The oracle is the ids this test itself seeded, in the order `record`
    // created them — which IS the queue's promised oldest-first, id-ascending
    // order. Deriving it from a second read instead would let one truncating
    // read validate another and quietly assert nothing.
    assert_eq!(
        seen, expected,
        "paging serves the seeded queue in its promised order"
    );
}

#[tokio::test]
async fn the_final_report_page_reports_no_further_cursor() {
    // A cursor on the last page would make the dashboard offer a "load more"
    // that returns nothing.
    let (_c, pool) = migrated_pool().await;
    let reports = AbuseReportRepo::new(pool.clone());
    reports
        .record("only report", None, None, 1_000)
        .await
        .expect("record report");

    let page = reports
        .list_unresolved_page(10, None)
        .await
        .expect("page the queue");
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.next, None);
}

#[tokio::test]
async fn a_resolved_report_never_appears_in_a_page() {
    // Paging must apply the same unresolved filter as the unpaged read.
    let (_c, pool) = migrated_pool().await;
    let reports = AbuseReportRepo::new(pool.clone());
    let admin = seed_account(&pool, "admin@example.com").await;
    let first = reports
        .record("resolved one", None, None, 1_000)
        .await
        .expect("record report");
    reports
        .record("open one", None, None, 2_000)
        .await
        .expect("record report");
    reports
        .resolve(first, admin, 3_000)
        .await
        .expect("resolve report");

    let page = reports
        .list_unresolved_page(10, None)
        .await
        .expect("page the queue");
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].body, "open one");
}

#[tokio::test]
async fn paging_the_audit_log_walks_every_row_exactly_once_newest_first() {
    // The audit log is newest-first. 3_000 appears twice, and newest-first at
    // limit=2 that pair is SPLIT by the first page boundary: page 1 ends on the
    // later 3_000 row and page 2 must resume at the earlier one. A
    // timestamp-only cursor (`occurred_at < $1`) excludes every 3_000 row there
    // and drops it, so this seed is what makes the `id` tiebreak observable.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "admin@example.com").await;

    let mut expected: Vec<Uuid> = Vec::new();
    for at in [1_000_u64, 2_000, 3_000, 3_000, 4_000] {
        expected.push(
            audit
                .append(
                    &AuditEntry {
                        actor_account_id: actor,
                        action: "view-reports".to_owned(),
                        target_type: None,
                        target_id: None,
                        metadata: None,
                        context_session_id: None,
                        context_definition_id: None,
                    },
                    at,
                )
                .await
                .expect("append audit entry"),
        );
    }
    // `append` writes ascending timestamps and ascending uuidv7 ids, so
    // newest-first `(occurred_at DESC, id DESC)` is exactly insertion order
    // reversed — including within the shared 3_000 pair.
    expected.reverse();

    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<PageCursor> = None;
    loop {
        let page = audit
            .list_page(2, cursor)
            .await
            .expect("page the audit log");
        assert!(page.rows.len() <= 2, "a page never exceeds its limit");
        seen.extend(page.rows.iter().map(|r| r.id));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    // Compared against the seeded ids, not against a second read: an oracle
    // that is itself a bounded read would compare one truncation with another.
    assert_eq!(
        seen, expected,
        "paging serves every appended row exactly once, newest first"
    );
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "no row is served twice");
}

// ---- audit-log investigation filters -------------------------------------------

/// Appends one audit row, returning its id.
async fn append_ctx(
    audit: &AuditLogRepo,
    actor: Uuid,
    action: &str,
    target: Option<Uuid>,
    session: Option<Uuid>,
    definition: Option<Uuid>,
    at: u64,
) -> Uuid {
    audit
        .append(
            &AuditEntry {
                actor_account_id: actor,
                action: action.to_owned(),
                target_type: target.map(|_| "account".to_owned()),
                target_id: target,
                metadata: None,
                context_session_id: session,
                context_definition_id: definition,
            },
            at,
        )
        .await
        .expect("append audit entry")
}

#[tokio::test]
async fn filtering_by_actor_returns_only_that_actors_rows() {
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let alice = seed_account(&pool, "alice@example.com").await;
    let bob = seed_account(&pool, "bob@example.com").await;

    let a1 = append_ctx(&audit, alice, "view-reports", None, None, None, 1_000).await;
    append_ctx(&audit, bob, "view-reports", None, None, None, 2_000).await;
    let a2 = append_ctx(&audit, alice, "view-audit-log", None, None, None, 3_000).await;

    let page = audit
        .list_page_filtered(50, None, Some(alice), None, None)
        .await
        .expect("filter by actor");
    let ids: Vec<Uuid> = page.rows.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![a2, a1], "only alice's rows, newest first");
}

#[tokio::test]
async fn filtering_by_object_reaches_a_row_through_its_target() {
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let admin = seed_account(&pool, "admin@example.com").await;
    let victim = seed_account(&pool, "victim@example.com").await;

    let disabled = append_ctx(
        &audit,
        admin,
        "disable-account",
        Some(victim),
        None,
        None,
        1_000,
    )
    .await;
    append_ctx(&audit, admin, "view-reports", None, None, None, 2_000).await;

    let page = audit
        .list_page_filtered(50, None, None, None, Some(victim))
        .await
        .expect("filter by object");
    assert_eq!(
        page.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![disabled]
    );
}

#[tokio::test]
async fn filtering_by_a_session_reaches_a_role_grant_whose_target_is_the_account() {
    // THE CRUX. A role grant's `target_id` is the GRANTEE ACCOUNT, not the
    // session — so a naive `target_id = $1` filter answers "what happened in
    // this session?" by silently omitting every role change. The context column
    // is what closes that hole.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let granter = seed_account(&pool, "ncs@example.com").await;
    let grantee = seed_account(&pool, "logger@example.com").await;
    let session = Uuid::now_v7();
    let definition = Uuid::now_v7();

    let grant = append_ctx(
        &audit,
        granter,
        "role-granted",
        Some(grantee),
        Some(session),
        Some(definition),
        1_000,
    )
    .await;
    // An unrelated session's grant must NOT come back.
    append_ctx(
        &audit,
        granter,
        "role-granted",
        Some(grantee),
        Some(Uuid::now_v7()),
        Some(Uuid::now_v7()),
        2_000,
    )
    .await;

    let page = audit
        .list_page_filtered(50, None, None, None, Some(session))
        .await
        .expect("filter by session");
    assert_eq!(
        page.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![grant],
        "the role grant is found by the session it happened in"
    );
}

#[tokio::test]
async fn filtering_by_a_net_reaches_events_of_its_sessions() {
    // "Who acted on this net?" must not require the admin to already know every
    // session id under it.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "owner@example.com").await;
    let definition = Uuid::now_v7();
    let session = Uuid::now_v7();

    let created = append_ctx(
        &audit,
        actor,
        "net-created",
        Some(definition),
        None,
        Some(definition),
        1_000,
    )
    .await;
    let started = append_ctx(
        &audit,
        actor,
        "session-started",
        Some(session),
        Some(session),
        Some(definition),
        2_000,
    )
    .await;
    append_ctx(
        &audit,
        actor,
        "net-created",
        None,
        None,
        Some(Uuid::now_v7()),
        3_000,
    )
    .await;

    let page = audit
        .list_page_filtered(50, None, None, None, Some(definition))
        .await
        .expect("filter by net");
    assert_eq!(
        page.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![started, created],
        "both the net's own event and its session's event, newest first"
    );
}

#[tokio::test]
async fn a_row_matching_both_union_branches_is_returned_once() {
    // `net-created` has target_id == context_definition_id, so it satisfies both
    // UNION ALL branches. Without the `IS DISTINCT FROM` dedup guard it would be
    // listed twice and inflate every count an admin reads.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "owner@example.com").await;
    let definition = Uuid::now_v7();

    append_ctx(
        &audit,
        actor,
        "net-created",
        Some(definition),
        None,
        Some(definition),
        1_000,
    )
    .await;

    let page = audit
        .list_page_filtered(50, None, None, None, Some(definition))
        .await
        .expect("filter by net");
    assert_eq!(page.rows.len(), 1, "no duplicate across the union branches");
}

#[tokio::test]
async fn filters_compose_and_page_without_gap_or_repeat() {
    // The cursor must stay total under a filter: a page boundary that dropped or
    // repeated a row would misreport an investigation.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "owner@example.com").await;
    let other = seed_account(&pool, "other@example.com").await;
    let definition = Uuid::now_v7();

    let mut expected: Vec<Uuid> = Vec::new();
    for (n, at) in [1_000_u64, 2_000, 2_000, 3_000, 4_000].iter().enumerate() {
        // Two rows share a millisecond — the id tiebreak is what keeps paging total.
        let session = if n % 2 == 0 {
            Some(Uuid::now_v7())
        } else {
            None
        };
        expected.push(
            append_ctx(
                &audit,
                actor,
                "session-started",
                None,
                session,
                Some(definition),
                *at,
            )
            .await,
        );
        // Noise the filter must exclude.
        append_ctx(
            &audit,
            other,
            "view-reports",
            None,
            None,
            Some(Uuid::now_v7()),
            *at,
        )
        .await;
    }
    expected.reverse(); // newest-first

    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor = None;
    loop {
        let page = audit
            .list_page_filtered(2, cursor, Some(actor), None, Some(definition))
            .await
            .expect("filtered page");
        assert!(page.rows.len() <= 2);
        seen.extend(page.rows.iter().map(|r| r.id));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen, expected, "every matching row exactly once, in order");
}

#[tokio::test]
async fn an_unfiltered_call_matches_the_plain_paged_read() {
    // The filtered query with all filters absent must be a drop-in for
    // `list_page` — otherwise the review surface's default view changes shape.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "admin@example.com").await;
    for at in [1_000_u64, 2_000, 3_000] {
        append_ctx(&audit, actor, "view-reports", None, None, None, at).await;
    }

    let filtered = audit
        .list_page_filtered(2, None, None, None, None)
        .await
        .expect("filtered");
    let plain = audit.list_page(2, None).await.expect("plain");
    assert_eq!(
        filtered.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        plain.rows.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    assert_eq!(filtered.next, plain.next);
}

#[tokio::test]
async fn audit_timestamps_carry_no_sub_millisecond_component() {
    // The keyset cursor round-trips through epoch MILLIS. That is exact only
    // because `append` writes `utc_from_millis`. If anything ever wrote
    // microsecond precision, the cursor would truncate down and silently drop
    // every row inside the truncated interval at a page boundary. Pin it.
    let (_c, pool) = migrated_pool().await;
    let audit = AuditLogRepo::new(pool.clone());
    let actor = seed_account(&pool, "admin@example.com").await;
    append_ctx(
        &audit,
        actor,
        "view-reports",
        None,
        None,
        None,
        1_754_000_000_123,
    )
    .await;

    let micros: i64 = sqlx::query_scalar(
        "SELECT (EXTRACT(MICROSECONDS FROM occurred_at)::bigint % 1000) FROM audit_log",
    )
    .fetch_one(&pool)
    .await
    .expect("read microsecond remainder");
    assert_eq!(
        micros, 0,
        "occurred_at is millisecond-precision by construction"
    );
}
