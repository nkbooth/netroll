---
title: "Upgrade an instance"
description: "Pin a NetRoll image version, upgrade to a new release, verify the boot, and roll back to the previous tag when an upgrade goes wrong."
docType: "howto"
audience: ["self-hoster"]
lastVerified: "2026-09-14"
---

# Upgrade an instance

Move a running instance to a new NetRoll release, and get back to the previous one if you need
to.

## Prerequisites

- [A running instance](quickstart.md).
- A current Postgres backup. Migrations run automatically at boot and are not reversed by
  rolling the image back.

## Steps

### Step 1: Pin a version

Pin an explicit tag rather than tracking `latest`, so an upgrade is something you choose:

```yaml
services:
    app:
        image: ${IMAGE_REF:-ghcr.io/nkbooth/netroll}:${IMAGE_TAG:-latest}
```

```bash
# .env
IMAGE_TAG=v1.4.0
```

Releases follow semantic versioning, derived from Conventional Commits. Check the
[CHANGELOG](https://github.com/nkbooth/netroll/blob/main/CHANGELOG.md) before moving
across a major version.

### Step 2: Back up Postgres

```bash
podman compose exec postgres pg_dump -U netroll netroll > netroll-$(date +%F).sql
```

Back up the avatar volume too if you care about uploaded avatars — they're the only user data
outside Postgres.

### Step 3: Set the new tag and pull

```bash
# .env
IMAGE_TAG=v1.5.0
```

```bash
podman compose pull
```

### Step 4: Recreate the container

```bash
podman compose up -d
```

The new container applies any pending schema migrations at boot before serving traffic.

## Verify

```bash
podman compose logs app | tail -30   # no migration or boot errors
curl -s http://localhost:8080/healthz   # {"status":"ok"} once it serves and reaches the database
curl -sI https://netroll.example.com/ | head -1
```

Sign in, open a net, and start a session. If the app is serving pages but a specific feature
returns HTTP 503, an optional variable it depends on is unset — the QRZ endpoints do exactly
that without a `KEK`.

### If the container exits with an index migration error

NetRoll builds every index after the initial schema with `CREATE INDEX CONCURRENTLY`, so an
upgrade never takes a write lock on your live net and session data. The initial schema's own
indexes are plain builds, which costs nothing: that migration runs once, against the empty
database you are creating. The trade-off of the concurrent form is that a build which fails
partway — a lost connection, a restart mid-build, a `statement_timeout` — leaves an **invalid**
index behind. NetRoll cannot clean that up for you: the migration is re-run verbatim on the next
boot, and it then fails against the leftover, so the container keeps exiting until you remove it.

The symptom in `podman compose logs app` is a boot abort naming a `_v2` index:

```text
error: while executing migration ...: relation "idx_<name>_v2" already exists
```

!!! warning "A slow migration is not a failed one — check before you drop anything"

    `CREATE INDEX CONCURRENTLY` waits for every transaction that was already open when it
    started, so one long-running query elsewhere makes a perfectly healthy upgrade look
    stalled. A build that is still waiting reports `indisvalid = f` in the detection query
    below — **exactly like a failed one**. Dropping it aborts a build that would have
    succeeded and manufactures the very leftover this section exists to remove.

    Only continue once the app container has **exited** and no concurrent build is still
    running. Nothing below is safe to act on before that.

Confirm nothing is still building:

```bash
podman compose ps                      # the app container must be Exited, not Up
podman compose exec postgres psql -U netroll -d netroll -c \
  "SELECT pid, state, query FROM pg_stat_activity
    WHERE query ILIKE '%CONCURRENTLY%' AND pid <> pg_backend_pid();"
```

An exited app container plus zero rows here means no build is in flight, and anything the next
query reports is a leftover rather than work in progress.

Find the leftovers:

```bash
podman compose exec postgres psql -U netroll -d netroll -c \
  "SELECT indexrelid::regclass AS index_name, indrelid::regclass AS table_name,
          indisvalid, indisready
     FROM pg_index WHERE NOT indisvalid;"
```

`indisready` says how far the build got. It narrows down *where* a build stopped, but on its
own it does not prove that it stopped — the check above is what establishes that:

| `indisvalid` | `indisready` | Reading |
|---|---|---|
| `f` | `f` | Never completed its first build pass. Either it failed there, or it is still waiting for open transactions to finish. |
| `f` | `t` | Completed the build pass and then failed — or is still busy — validating rows. |

With the app container exited and no `CONCURRENTLY` query running, drop each index the query
reports. This is safe to run against a live database — it takes no lock that blocks reads or
writes:

```bash
podman compose exec postgres psql -U netroll -d netroll -c \
  "DROP INDEX CONCURRENTLY IF EXISTS idx_<name>_v2;"
```

Then start the container again. The migration re-runs from a clean slate:

```bash
podman compose up -d
```

If the query returns no rows, the boot failure is something other than a half-built index —
read the full log rather than applying this fix.

### If the container exits with an extension error

The initial schema creates the `pg_trgm` extension, which the title-search indexes are built
on, so this failure happens on a NEW instance's first boot rather than partway through an
upgrade.
The app cannot work around a database that refuses it, so the boot stops there. This is a
**different failure from a half-built index**: the detection query above returns no rows, there
is nothing to drop, and dropping anything would be wrong. The remedy is a privilege or a provider
setting, then a restart.

The symptom in `podman compose logs app` is one of two lines:

```text
error: while executing migration ...: permission denied to create extension "pg_trgm"
```

The role the app connects as lacks `CREATE` on the database. This cannot happen on the
quickstart stack — there `netroll` is the superuser the image created — so you are on a database
where the app's role was created separately with fewer rights. On PostgreSQL 13 or newer,
`pg_trgm` is a trusted extension, so `CREATE` on the database is all it needs. Grant it as a
role that can (`<superuser>` below is whatever your server calls that role) and restart:

```bash
podman compose exec postgres psql -U <superuser> -d netroll -c \
  "GRANT CREATE ON DATABASE netroll TO netroll;"
podman compose up -d
```

On PostgreSQL 12 or older, only a superuser can create the extension. Create it yourself once
and restart — the migration is written `IF NOT EXISTS`, so it then succeeds without doing
anything:

```bash
podman compose exec postgres psql -U <superuser> -d netroll -c \
  "CREATE EXTENSION IF NOT EXISTS pg_trgm;"
podman compose up -d
```

```text
error: while executing migration ...: could not open extension control file ".../pg_trgm.control"
```

The extension's files are not installed on the server at all. The official `postgres` image
ships them; a distribution package usually puts them in a separate `contrib` package; a managed
provider exposes them through its own extension allow-list. Install or enable `pg_trgm` there,
then restart. There is no in-app setting that turns the requirement off.

## Roll back

Set `IMAGE_TAG` back to the previous version and bring the stack up again:

```bash
# .env
IMAGE_TAG=v1.4.0
```

```bash
podman compose up -d
```

Rolling the image back does **not** roll back schema migrations. If the release you're leaving
introduced a migration the older binary can't read, restore your Postgres dump as well.

## Related guides

- [Deployment model](deployment-model.md)
- [Configuration reference](../reference/configuration.md)
- [Troubleshooting self-hosting](../troubleshooting/self-hosting.md)
- [Releases](https://github.com/nkbooth/netroll/blob/main/CONTRIBUTING.md#releases) in CONTRIBUTING.md — how versions get cut
