// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! A plain `CREATE INDEX` is legal only in the initial-schema migration, which
//! runs against an empty database; every later one runs against live data and
//! must be `-- no-transaction` plus one `CREATE INDEX CONCURRENTLY`.
//! ⚠️ A NEW `.sql` FILE DOES NOT REBUILD THIS BINARY — `sqlx::migrate!` creates
//! no dependency edge on the directory — so touch this file first.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

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

/// The one migration whose indexes may be plain-form: it runs against an empty
/// database and nothing else ever will. Every later migration runs against live
/// data.
const INITIAL_SCHEMA_VERSION: i64 = 20260715000000;

/// Indexes whose exact shape is load-bearing, checked against the live catalog
/// after the migration set has run. Because the schema is now built by one
/// generated file, these are its fidelity check: a lost `UNIQUE` or a dropped
/// partial predicate is a silent correctness or performance regression and
/// shows up here rather than in production.
const SHAPE_CRITICAL_INDEXES: [&str; 4] = [
    "idx_accounts_pending_deletion",
    "idx_net_definitions_link_token",
    "idx_net_occurrences_scheduled_start_at",
    "idx_session_events_actor",
];

/// The `WHERE NOT indisvalid` detection semantics documented for operators in
/// `docs/self-hosting/upgrade-an-instance.md` and `backend/migrations/README.md`,
/// narrowed to a single text column so the assertions can compare index names
/// directly. The predicate matches what those pages tell an operator to run; the
/// projection deliberately does not, and nothing couples the two — editing the
/// documented query will not fail this test. What is covered is the behaviour
/// the docs rest on: that `NOT indisvalid` surfaces a failed concurrent build
/// and stops surfacing it once the documented repair has run.
const INVALID_INDEX_DETECTION: &str =
    "SELECT indexrelid::regclass::text FROM pg_index WHERE NOT indisvalid";

/// Strip `--` line comments so an assertion about SQL *statements* is not
/// satisfied (or tripped) by prose. A migration may discuss
/// `CREATE INDEX CONCURRENTLY` in its rationale header while being a plain,
/// transactional migration, so a naive substring test over raw file text would
/// report the exact opposite of the truth.
fn statements_only(sql: &str) -> String {
    sql.lines()
        .map(|line| match line.find("--") {
            Some(start) => &line[..start],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Number of SQL statements in a migration, counting a trailing semicolon once.
fn statement_count(sql: &str) -> usize {
    statements_only(sql)
        .split(';')
        .filter(|fragment| !fragment.trim().is_empty())
        .count()
}

/// A plain-form `CREATE INDEX` found in one migration.
#[derive(Debug, PartialEq, Eq)]
struct PlainIndex {
    index: String,
    /// The statement, whitespace-normalised, so a failure names what was
    /// written and not only where.
    statement: String,
}

/// Every `CREATE [UNIQUE] INDEX` in `sql` whose next word is not
/// `CONCURRENTLY`. Walks the token stream rather than statement boundaries so
/// it can be pointed at raw file text as well as at `statements_only` output —
/// the comment-blindness proof depends on the two disagreeing.
///
/// `origin` names what is being scanned, so the malformed-migration panic
/// names the file rather than only the tokens that confused it.
fn plain_indexes_in(sql: &str, origin: &str) -> Vec<PlainIndex> {
    let tokens: Vec<&str> = sql.split_whitespace().collect();
    let word_at =
        |at: usize, word: &str| tokens.get(at).is_some_and(|t| t.eq_ignore_ascii_case(word));

    let mut found = Vec::new();
    for (start, token) in tokens.iter().enumerate() {
        if !token.eq_ignore_ascii_case("CREATE") {
            continue;
        }
        let mut cursor = start + 1;
        if word_at(cursor, "UNIQUE") {
            cursor += 1;
        }
        if !word_at(cursor, "INDEX") {
            continue;
        }
        cursor += 1;
        if word_at(cursor, "CONCURRENTLY") {
            continue;
        }
        // No migration uses `IF NOT EXISTS` on an index today; skipped so a
        // future one cannot hide a plain index behind the clause.
        if word_at(cursor, "IF") {
            cursor += 3;
        }
        let index = tokens.get(cursor).unwrap_or_else(|| {
            panic!(
                "{origin}: malformed CREATE INDEX with no index name: `{}`",
                tokens[start..].join(" ")
            )
        });
        let end = tokens[start..]
            .iter()
            .position(|t| t.ends_with(';'))
            .map_or(tokens.len(), |offset| start + offset + 1);
        found.push(PlainIndex {
            index: (*index).to_string(),
            statement: tokens[start..end].join(" "),
        });
    }
    found
}

/// The on-disk name sqlx parsed a migration from. sqlx keeps the version and
/// turns the filename's underscores into spaces for `description`, so the
/// round trip is exact for this tree, where no filename contains a space.
fn migration_file(migration: &sqlx::migrate::Migration) -> String {
    format!(
        "{}_{}.sql",
        migration.version,
        migration.description.replace(' ', "_")
    )
}

/// Every plain-form index in the migration set, with the file that builds it.
fn plain_indexes_in_migrations() -> Vec<(i64, String, PlainIndex)> {
    sqlx::migrate!("../../migrations")
        .iter()
        .flat_map(|migration| {
            plain_indexes_in(
                &statements_only(migration.sql.as_str()),
                &migration_file(migration),
            )
            .into_iter()
            .map(move |plain| (migration.version, migration_file(migration), plain))
        })
        .collect()
}

#[test]
fn a_plain_create_index_outside_the_initial_schema_fails_naming_its_migration() {
    // The exemption below is only meaningful if the initial schema is actually in
    // the resolved set and sorts first: a migration back-dated below it would
    // otherwise inherit the exemption silently.
    let versions: Vec<i64> = sqlx::migrate!("../../migrations")
        .iter()
        .map(|migration| migration.version)
        .collect();
    assert_eq!(
        versions.first(),
        Some(&INITIAL_SCHEMA_VERSION),
        "the initial schema must be the lowest version in the resolved set: {versions:?}"
    );

    let offenders: Vec<String> = plain_indexes_in_migrations()
        .into_iter()
        .filter(|(version, _, _)| *version != INITIAL_SCHEMA_VERSION)
        .map(|(_, file, plain)| format!("{file}: {}", plain.statement))
        .collect();

    assert!(
        offenders.is_empty(),
        "plain CREATE INDEX outside the initial schema. The initial schema may build indexes \
         plainly because it only ever runs against an empty database; every migration after it \
         runs against live data and is `-- no-transaction` + CREATE INDEX CONCURRENTLY, alone in \
         its file (migrations/README.md). There is no exception to add — converting a plain build \
         is a new three-file rebuild, not an entry in a list:\n{}",
        offenders.join("\n")
    );
}

/// The scan reads statements, not prose: a migration that complies while its
/// rationale header discusses a plain `CREATE INDEX` must not red the suite.
///
/// This used to pin a real migration whose header said exactly that. No such
/// file survives the squash, and the property under test never needed one — it
/// is `plain_indexes_in` composed with `statements_only`, both pure. The
/// premise assertion is what makes it a proof rather than a tautology: the raw
/// scan MUST report a hit, or the stripped scan reporting none says nothing.
#[test]
fn the_plain_index_scan_is_blind_to_comments() {
    const COMMENTED_MIGRATION: &str = "\
-- no-transaction
--
-- A plain CREATE INDEX idx_example ON example (id) would be harmless here, the
-- table being empty — this migration complies with the convention anyway.
CREATE INDEX CONCURRENTLY idx_example ON example (id);
";

    let naive = plain_indexes_in(COMMENTED_MIGRATION, "the commented-migration fixture");
    assert!(
        !naive.is_empty(),
        "premise: a raw-text scan of the fixture must report a plain index (the one named in the \
         comment) or this test proves nothing"
    );

    let shipped = plain_indexes_in(
        &statements_only(COMMENTED_MIGRATION),
        "the commented-migration fixture",
    );
    assert!(
        shipped.is_empty(),
        "the fixture has one statement and it is CONCURRENTLY; the scan reported {shipped:?}"
    );
}

/// `statements_only` strips `--` line comments and nothing else, and the
/// plain-index scan depends on it in BOTH directions. A block comment breaks it
/// either way — `CREATE /* fast path */ INDEX foo` leaves no `CREATE`-then-
/// `INDEX` adjacency, so a genuine plain index scans clean; and a
/// `/* … CREATE INDEX … */` rationale block is read as a real statement,
/// reddening the suite at prose. Rather than rewrite a helper four other
/// assertions in this file depend on, the syntax is forbidden here — the
/// migrations use `--` throughout (`pg_dump` emits `--` only), so this costs
/// nothing and closes both directions at source. `migrations/README.md` carries
/// the rule for authors.
#[test]
fn no_migration_uses_a_block_comment() {
    let carriers: Vec<String> = sqlx::migrate!("../../migrations")
        .iter()
        .filter(|migration| migration.sql.as_str().contains("/*"))
        .map(migration_file)
        .collect();

    assert!(
        carriers.is_empty(),
        "block comments are not used in this migration set, because the plain-index scan \
         cannot see through them in either direction (a plain index hides behind one, and \
         prose inside one reads as a statement). Use `--` line comments:\n{}",
        carriers.join("\n")
    );
}

/// The convention cannot be reintroduced wrongly: any migration reaching for
/// `CONCURRENTLY` must be non-transactional and alone in its file, or it fails
/// at boot rather than in review.
#[test]
fn a_concurrent_index_migration_is_non_transactional_and_alone_in_its_file() {
    for migration in sqlx::migrate!("../../migrations").iter() {
        let statements = statements_only(migration.sql.as_str()).to_uppercase();
        if !statements.contains("CONCURRENTLY") {
            continue;
        }
        assert!(
            migration.no_tx,
            "migration {} ({}) uses CONCURRENTLY but is transactional — sqlx only skips its \
             BEGIN when the file's first bytes are `-- no-transaction`, and CREATE INDEX \
             CONCURRENTLY is rejected inside a transaction block",
            migration.version, migration.description
        );
        assert_eq!(
            statement_count(migration.sql.as_str()),
            1,
            "migration {} ({}) uses CONCURRENTLY and must therefore be the only statement in its \
             file — sqlx sends the whole file as one simple query and Postgres wraps a \
             multi-statement simple query in an implicit transaction, which re-imposes the very \
             constraint `-- no-transaction` lifts",
            migration.version,
            migration.description
        );
    }
}

#[tokio::test]
async fn the_shape_critical_indexes_exist_and_are_valid_after_migrations() {
    let (_container, pool) = migrated_pool().await;

    for index in SHAPE_CRITICAL_INDEXES {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_index i
               JOIN pg_class c ON c.oid = i.indexrelid
              WHERE c.relname = $1 AND i.indisvalid AND i.indisready",
        )
        .bind(index)
        .fetch_one(&pool)
        .await
        .expect("query pg_index");

        assert_eq!(
            live, 1,
            "{index} must exist and be valid+ready after the migration set has run — a missing \
             row means the schema file no longer builds it"
        );
    }
}

#[tokio::test]
async fn the_shape_critical_indexes_keep_their_shape() {
    let (_container, pool) = migrated_pool().await;

    // Each index's load-bearing property. Losing any of these is a silent
    // correctness or performance regression, not a cosmetic difference — the
    // UNIQUE on link_token is the ONLY enforcement of link-token uniqueness.
    let required_fragments: [(&str, &str); 4] = [
        ("idx_net_definitions_link_token", "UNIQUE"),
        ("idx_accounts_pending_deletion", "deleted_at IS NOT NULL"),
        ("idx_session_events_actor", "actor IS NOT NULL"),
        (
            "idx_net_occurrences_scheduled_start_at",
            "(scheduled_start_at)",
        ),
    ];

    for (index, fragment) in required_fragments {
        let definition: String =
            sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE indexname = $1")
                .bind(index)
                .fetch_one(&pool)
                .await
                .unwrap_or_else(|error| panic!("read indexdef for {index}: {error}"));

        assert!(
            definition.contains(fragment),
            "{index} lost `{fragment}` — its definition is now `{definition}`"
        );
    }
}

#[tokio::test]
async fn migrations_leave_no_invalid_or_temporary_indexes() {
    let (_container, pool) = migrated_pool().await;

    let invalid: Vec<String> = sqlx::query_scalar(INVALID_INDEX_DETECTION)
        .fetch_all(&pool)
        .await
        .expect("run the documented invalid-index detection query");

    assert!(
        invalid.is_empty(),
        "the migration set left invalid indexes behind: {invalid:?}"
    );

    let leftovers: Vec<String> = sqlx::query_scalar(
        "SELECT relname FROM pg_class WHERE relkind = 'i' AND relname LIKE '%\\_v2'",
    )
    .fetch_all(&pool)
    .await
    .expect("query pg_class for temporary index names");

    assert!(
        leftovers.is_empty(),
        "the transient `_v2` rebuild names must not survive a rename migration: {leftovers:?}"
    );
}

#[tokio::test]
async fn the_documented_invalid_index_repair_actually_works() {
    let (_container, pool) = migrated_pool().await;

    // Reproduce the failure mode this is about: a concurrent unique build that
    // cannot succeed. Each statement runs on its own execute() call — the
    // one-statement rule that governs the migrations governs this test too.
    sqlx::query("CREATE TABLE repair_scratch (v integer)")
        .execute(&pool)
        .await
        .expect("create scratch table");
    sqlx::query("INSERT INTO repair_scratch (v) VALUES (1), (1)")
        .execute(&pool)
        .await
        .expect("seed duplicate values");

    let failed_build =
        sqlx::query("CREATE UNIQUE INDEX CONCURRENTLY repair_scratch_v_uq ON repair_scratch (v)")
            .execute(&pool)
            .await;
    assert!(
        failed_build.is_err(),
        "a concurrent UNIQUE build over duplicate values must fail — the premise here"
    );

    let detected: Vec<String> = sqlx::query_scalar(INVALID_INDEX_DETECTION)
        .fetch_all(&pool)
        .await
        .expect("run the documented detection query");
    assert_eq!(
        detected,
        vec!["repair_scratch_v_uq".to_string()],
        "the documented detection query must surface the leftover invalid index"
    );

    sqlx::query("DROP INDEX CONCURRENTLY IF EXISTS repair_scratch_v_uq")
        .execute(&pool)
        .await
        .expect("run the documented repair statement");

    let after_repair: Vec<String> = sqlx::query_scalar(INVALID_INDEX_DETECTION)
        .fetch_all(&pool)
        .await
        .expect("re-run the documented detection query");
    assert!(
        after_repair.is_empty(),
        "the documented repair must clear the invalid index so the migration can re-run on the \
         next boot: {after_repair:?}"
    );
}
