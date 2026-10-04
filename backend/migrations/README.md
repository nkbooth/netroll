# Migrations

sqlx migrations, applied by the app itself at boot
(`backend/crates/netroll-app/src/main.rs`). There is no separate migrate step and
no `sqlx-cli` in the deploy path — a failed migration aborts startup rather than
serving against a stale schema.

This file is not a migration. sqlx only picks up files matching
`<version>_<description>.sql`, so a `README.md` here is ignored by the resolver.

## Naming

`YYYYMMDDHHMMSS_snake_case_description.sql`. Indexes are named
`idx_<table>_<cols>`.

## Forward-only, and never edit an applied migration

sqlx stores a SHA-384 checksum per applied migration. A mismatch aborts
`run()`, which for this binary means **the instance refuses to boot**. Changing
an already-applied file — including adding a comment to it — is therefore a
production outage, not a tidy-up. Fix a migration by adding a new one.

## Index creation: `CONCURRENTLY` in every new migration

A plain `CREATE INDEX` holds a write lock on the table for the whole build. On a
populated `session_events` that stalls live session logging mid-net. Every
**new** index-creating migration uses `CREATE INDEX CONCURRENTLY`, which needs
two mechanical things to be true:

1. **The file's first bytes must be exactly `-- no-transaction`.** sqlx decides
   with a literal `sql.starts_with("-- no-transaction")` test. Not after a blank
   line, not after a rationale header, not after a BOM. This repo's usual habit
   of leading with a rationale comment produces a silently *transactional*
   migration that then fails at runtime with `CREATE INDEX CONCURRENTLY cannot
   run inside a transaction block`. Put the rationale *below* the directive.
2. **A `CONCURRENTLY` statement must be alone in its file.** `-- no-transaction`
   is necessary but not sufficient: sqlx sends a migration's whole SQL as one
   simple-query message, and Postgres wraps a multi-statement simple query in an
   *implicit* transaction, which re-imposes the constraint the directive lifts.
   `BEGIN`/`COMMIT` workarounds and `DO $$ … $$` blocks do not escape this.
   Comments are free — they are not statements.

Also expect `CREATE INDEX CONCURRENTLY` to **wait for every transaction open at
its start to finish**. Against a live instance a long-open transaction makes the
migration look like it is hanging. That is the documented behaviour, not a bug.

The rule has exactly one exception, and it is enforced rather than described:

> A plain-form `CREATE INDEX` is legal only in the initial-schema migration.
> Every other migration is `-- no-transaction` plus exactly one
> `CREATE INDEX CONCURRENTLY`.

The initial schema builds its indexes plainly because it only ever runs against
an empty database, where the write lock it takes is on a table nobody can be
reading or writing. Every migration after it runs against live data, so there is
no exception to claim and nothing to add to a list — a new index-creating
migration is `CONCURRENTLY`, unconditionally, even on a table the same migration
set has just created empty. Changing the shape of one of the initial schema's
indexes on a live instance is the three-step rebuild below, never an edit to the
initial schema.

`backend/crates/netroll-adapters/tests/schema_concurrent_index_convention.rs`
scans every migration's *statements* (comments are stripped first, so prose
about the plain form does not trip it) and fails the suite on any plain
`CREATE INDEX` outside the initial schema, naming the migration file and the
statement. It also fails any migration that reaches for `CONCURRENTLY` while
being transactional or sharing its file with another statement — the two
mechanical requirements above, checked rather than remembered.

**Use `--` line comments, never `/* … */`.** A block comment defeats the scan in
both directions — a plain index written as `CREATE /* note */ INDEX` is invisible
to it, and a rationale block that merely *mentions* a plain index is read as one
— so `no_migration_uses_a_block_comment` in the same test file forbids the
syntax outright rather than teaching the stripper a second grammar.

**⚠️ A new `.sql` file does not rebuild the test binary that enforces this.**
`sqlx::migrate!` embeds one `include_str!` per migration it already resolved, so
it notices edits to existing migrations and creates no dependency on the
directory; there is no `build.rs` here to add one. On a warm `target/`, adding a
migration and running the suite proves nothing — the check still holds the
previous set and reports green for exactly the case it exists to catch. `touch`
`backend/crates/netroll-adapters/tests/schema_concurrent_index_convention.rs`
before you trust a local run. CI builds cold, so the gate itself is unaffected.

## Extensions

A `CREATE EXTENSION` is an ordinary transactional statement, and that is exactly why it gets a
file of its own: it cannot share one with a `CREATE INDEX CONCURRENTLY` (rule 2 above — one
statement per `-- no-transaction` file), and it must **sort earlier by filename** than any
index that names one of its operator classes, because `gin_trgm_ops` does not resolve until the
extension exists. Write it `IF NOT EXISTS` so a re-run and a pre-provisioned database both
boot. Write it **unqualified** — no `WITH SCHEMA`, and no schema prefix on its
operator classes in the indexes that use it — so a database that already holds
the extension in another schema still resolves them through `search_path`
instead of failing on the first index. The initial schema already creates
`pg_trgm` this way, ahead of the trigram indexes that name `gin_trgm_ops`; a NEW
extension still gets its own file, sorting earlier than any index that depends
on it.

An extension is also a requirement on the *operator's* database — the files must be present
and the role must be allowed to create it — so adding one is a change to `docs/self-hosting/`
as well as a migration. `pg_trgm` is trusted from PostgreSQL 13 on (the database owner is
enough); older majors need a superuser.

## Rebuilding an index that already exists

You cannot rebuild in place: two indexes cannot share a name, and
`CREATE INDEX CONCURRENTLY … IF NOT EXISTS` would no-op against the existing one
and rebuild nothing. Use three steps, in this order, one statement per file:

1. **Build** the replacement concurrently under a `_v2` name. Copy the original
   definition exactly — a lost `UNIQUE` or partial `WHERE` predicate is a silent
   correctness regression. Use `_v2`, not `_ccnew`/`_ccold`, which Postgres
   reserves for `REINDEX CONCURRENTLY`'s own leftovers.
2. **Drop** the original with `DROP INDEX CONCURRENTLY IF EXISTS`. Never before
   its build — dropping first leaves the column unindexed and, for a unique
   index, leaves the uniqueness rule unenforced.
3. **Rename** `_v2` back, in an ordinary *transactional* migration. `ALTER INDEX
   … RENAME` is metadata-only, transaction-safe and takes only
   `SHARE UPDATE EXCLUSIVE`, so several renames belong together in one file for
   atomicity.

The set this repo last did this to — `idx_accounts_pending_deletion`,
`idx_net_definitions_link_token`, `idx_net_occurrences_scheduled_start_at` and
`idx_session_events_actor` — is history now that the migration set has been
folded into one initial schema; the worked example lives in the project's
pre-1.0 private history, not in this repository. The three steps above are the
recipe.

## When a concurrent build fails

A failed `CREATE INDEX CONCURRENTLY` **leaves an INVALID index behind**, and
sqlx never records a failed migration, so the same file re-runs on the next boot
and fails with `relation "…_v2" already exists` — wedging startup until the
leftover is removed.

This is **not self-healing, by construction**: a pre-emptive
`DROP … IF EXISTS` cannot share the file (rule 2 above), and putting it in an
earlier migration does not help because that migration is already recorded and
never re-runs. The remedy is an operator action.

**Establish that the build has actually stopped before dropping anything.** A
concurrent build that is merely *waiting* on an open transaction reports
`indisvalid = f` exactly like a failed one, so the detection query alone cannot
tell them apart — dropping a healthy in-flight build creates the very leftover
this section is about. The app container must have exited, and no backend may
still be running the statement:

```sql
SELECT pid, state, query
  FROM pg_stat_activity
 WHERE query ILIKE '%CONCURRENTLY%'
   AND pid <> pg_backend_pid();
```

Detect:

```sql
SELECT indexrelid::regclass AS index_name,
       indrelid::regclass   AS table_name,
       indisvalid,
       indisready
  FROM pg_index
 WHERE NOT indisvalid;
```

`indisready` narrows down where the build stopped — `f`/`f` never finished the
first build pass, `f`/`t` finished it and then failed (or is still) validating —
but neither value proves the build has stopped. Only the check above does.

Repair — safe on a live instance, then restart so the migration re-runs from a
clean slate:

```sql
DROP INDEX CONCURRENTLY IF EXISTS idx_<name>_v2;
```

The operator-facing version of this lives in
[`docs/self-hosting/upgrade-an-instance.md`](../../docs/self-hosting/upgrade-an-instance.md);
keep the two in step when either changes.

`backend/crates/netroll-adapters/tests/schema_concurrent_index_convention.rs`
exercises the `WHERE NOT indisvalid` detection semantics and the
`DROP INDEX CONCURRENTLY IF EXISTS` repair end to end against a real Postgres,
and asserts the migration set leaves zero invalid indexes and zero `_v2`
leftovers behind. It does **not** execute this page's SQL verbatim — it runs its
own narrower projection of the same query — so a change to the columns selected
here is not caught by any test.
