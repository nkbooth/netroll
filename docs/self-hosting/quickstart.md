---
title: "Self-hosting quickstart"
description: "Stand up your own NetRoll instance from a compose file: the app container, Postgres, the environment variables the app refuses to boot without, and a first sign-in."
docType: "quickstart"
audience: ["self-hoster"]
lastVerified: "2026-09-14"
---

# Self-hosting quickstart

Get a working NetRoll instance running, sign in to it, and confirm email delivery works.

**Time:** ~10 minutes

## Prerequisites

- A host with Podman or Docker and compose support.
- An SMTP relay you can send through, and a sender address on a domain it's authorized for.
  Sign-in is a magic link, so without working email nobody can log in.
- A hostname and a reverse proxy terminating TLS in front of the instance.
- PostgreSQL 13 or newer with the `pg_trgm` extension available. The app creates the extension
  itself at boot and refuses to start without it. The compose file below covers this: the
  official `postgres:18` image ships `pg_trgm`, and the `netroll` role it creates owns the
  database, which is all the permission a trusted extension needs. On a Postgres you don't
  operate, check the provider's extension allow-list first — see
  [Upgrade an instance](upgrade-an-instance.md#if-the-container-exits-with-an-extension-error).

## Steps

### Step 1: Write the compose file

```yaml
services:
    app:
        image: ghcr.io/nkbooth/netroll:latest
        restart: unless-stopped
        env_file: .env
        ports:
            - '8080:3000' # host:container — the container side must equal PORT
        volumes:
            # Uploaded avatars: the only user data outside Postgres.
            - netroll-avatars:/app/data/avatars
        depends_on:
            - postgres
        # Podman ignores the published image's own HEALTHCHECK, so it's repeated here.
        healthcheck:
            test: ['CMD', '/usr/bin/curl', '-fsS', '-o', '/dev/null', 'http://127.0.0.1:3000/healthz']
            interval: 30s
            timeout: 3s
            start_period: 30s
            retries: 3

    postgres:
        image: postgres:18
        restart: unless-stopped
        environment:
            POSTGRES_USER: netroll
            POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
            POSTGRES_DB: netroll
        volumes:
            - netroll-pgdata:/var/lib/postgresql

volumes:
    netroll-avatars:
    netroll-pgdata:
```

To build the image yourself instead of pulling it, run `podman build -f Containerfile .`. The
shipped `deploy/compose.prod.yml` is the same stack with a different published port — it uses
`3001:3000`, so proxy to `3001` rather than `8080` if you copy that file instead of the one
above — and an image repository you can override with `IMAGE_REF`; [`deploy/README.md`](https://github.com/nkbooth/netroll/blob/main/deploy/README.md)
is the file of record for what each shipped file is and for every variable the templates set.

### Step 2: Write the `.env`

Four values are required. The app refuses to boot without them rather than falling back to
something wrong:

```bash
PORT=3000
DATABASE_URL=postgres://netroll:CHANGE_ME@postgres:5432/netroll
POSTGRES_PASSWORD=CHANGE_ME
MAIL_FROM=Your Net <no-reply@example.com>
PUBLIC_BASE_URL=https://netroll.example.com

SMTP_HOST=smtp.example.com
SMTP_PORT=587
SMTP_USERNAME=...
SMTP_PASSWORD=...
```

`PUBLIC_BASE_URL` is the origin emailed magic links point at. Set it to the public URL your
reverse proxy serves, not to the container's address.

Everything else is optional and off by default. See the
[configuration reference](../reference/configuration.md) for the full set, including QRZ
credential encryption, bot mitigation, resource caps, and admin accounts.

### Step 3: Start it

```bash
podman compose up -d
```

The app applies its own schema migrations at boot. There's no separate migration step.

### Step 4: Point your proxy at it

Proxy your public hostname to the published port — `8080` in the compose file above. Terminate
TLS at the proxy. Do not set a `Content-Security-Policy` at the proxy — the app sends one itself,
and two are enforced together; a starting point is
[`deploy/reverse-proxy/Caddyfile.example`](https://github.com/nkbooth/netroll/blob/main/deploy/reverse-proxy/Caddyfile.example).

The container side of the port mapping must equal `PORT`. If you change `PORT`, change the
mapping too, or the host port forwards to nothing.

### Step 5: Sign in

Open your public URL, request a magic link, and open the emailed link.

To give yourself the bounded admin surface — viewing and resolving abuse reports, disabling and
re-enabling accounts — add your address to `ADMIN_ACCOUNT_EMAILS` and restart. It grants nothing
until an account exists under that address.

## Verify

```bash
curl -sI https://netroll.example.com/ | head -1   # expect HTTP/2 200
curl -s http://localhost:8080/healthz             # expect {"status":"ok"}
podman compose logs app | tail -20                # no boot errors
podman compose ps                                 # both services up, app (healthy)
```

Then sign in through the browser and confirm the magic-link email arrives. If the app booted but
no mail arrives, the problem is your SMTP settings or your sender domain, not NetRoll — see
[troubleshooting self-hosting](../troubleshooting/self-hosting.md).

## Next steps

- **[Deployment model](deployment-model.md)** — what state exists and what to back up.
- **[Configuration](../reference/configuration.md)** — every variable and its default.
- **[Upgrade an instance](upgrade-an-instance.md)** — pinning versions and rolling back.
- **[Enable analytics](enable-analytics.md)** — optional, and off until you set it.
