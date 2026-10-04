# Deploying NetRoll

What is in this directory and at the repository root, for someone who wants to run their own
NetRoll instance. The narrative walkthrough is the
[self-hosting quickstart](../docs/self-hosting/quickstart.md); this file is the record of what
each shipped file is, which one you copy, every environment variable the templates set, and
how the reverse proxy and the app split the work.

## The files

| File | What it is |
|---|---|
| `Containerfile` | The image recipe. `podman build -f Containerfile .` builds the Rust binary, the frontend and the documentation into one image. |
| `compose.yaml` (root) | Development backing services only: PostgreSQL and the Mailpit mail catcher. It runs no app container. Not a deployment file. |
| `deploy/compose.prod.yml` | The app stack: an `app` service on the published image and a `postgres` service. This is the file a self-hoster copies. |
| `.env.template` (root) | Every variable the two templates set, with a local-development value in plain text. Start from it when writing your own `.env`. A handful the app reads but neither template sets — `STATIC_DIR`, `CSP_POLICY`, `CSP_REPORT_ONLY` — are in the [configuration reference](../docs/reference/configuration.md). |
| `deploy/.env.production.tpl` | The same variables for the GitHub workflow: secrets and instance values as 1Password `op://netroll-deploy/…` references, policy values in plain text. Rendered with `op inject`; inert without a 1Password service account. |
| `deploy/rollout.sh` | The host side of the GitHub workflow: swaps in a new `.env` and compose file, waits for `/healthz`, and rolls back both when the new release never turns healthy. |
| `deploy/reverse-proxy/Caddyfile.example` | A Caddy 2 front door that terminates TLS, sets the transport headers, and deliberately sets no Content Security Policy. |

## Run it

1. Copy `deploy/compose.prod.yml` to a directory on the host and write a `.env` beside it.
2. Set the values the app refuses to boot without: `DATABASE_URL`, `MAIL_FROM`,
   `PUBLIC_BASE_URL` and `POSTGRES_PASSWORD` (the password in `DATABASE_URL` and
   `POSTGRES_PASSWORD` must agree, since the compose file hands the second to PostgreSQL).
   Sign-in is a magic link, so set the `SMTP_*` values too or nobody can log in.
3. Check the port line. The shipped file publishes `3001:3000`. The host side is yours to
   change; the container side must equal `PORT`, which defaults to `3000`.
4. `podman compose -f compose.prod.yml up -d`. The app applies its own schema at boot; there is
   no separate migration step. `curl -s http://localhost:3001/healthz` prints
   `{"status":"ok"}` once it is serving and can reach the database.
5. Point your reverse proxy at the published port and terminate TLS there. Open your public
   URL, request a magic link, and open the emailed link.

Every variable is described in the [configuration reference](../docs/reference/configuration.md).

## Environment variables

The table names every variable the two templates set. `scripts/check-env-docs.sh` compares its
first column with the templates and exits non-zero when they disagree; run it after adding a
variable to a template, and add the row here in the same change. The `Required` column says whether the stack refuses to
start without a value — for every variable but `POSTGRES_PASSWORD` that means the app's own
boot check; `POSTGRES_PASSWORD` is required by the `postgres` service, and the app only ever
sees it inside `DATABASE_URL`.

<!-- env-table:start -->
| Variable | Required | What it is |
|---|---|---|
| `ADMIN_ACCOUNT_EMAILS` | no | Comma-separated emails granted the admin surface once an account exists under them. Empty means no admins. |
| `AVATAR_DIR` | no | Directory uploaded avatars are written to. Baked into the image as `/app/data/avatars`; mount a volume there. |
| `BOT_MITIGATION_SECRET` | no | HMAC secret for the signup and net-creation form tokens. Empty disables the mitigation; at least 16 bytes when set. |
| `DATABASE_URL` | yes | PostgreSQL connection URL. In the compose stack the host is `postgres`. |
| `KEK` | no | Base64 32-byte key that seals stored QRZ credentials. Empty leaves QRZ features off. Not rotatable: every stored credential is sealed under it. |
| `KOFI_USERNAME` | no | Ko-fi username behind the footer support link. Empty renders no link. |
| `MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR` | no | Instance-wide ceiling on magic-link email per hour. Default 60. |
| `MAGIC_LINK_RESERVE_SENDS_PER_HOUR` | no | Reserve budget for addresses that signed in recently, drawn on once the ceiling is spent. Default 120. |
| `MAIL_FROM` | yes | The From address on every outgoing email. |
| `MAX_NETS_PER_USER` | no | Active nets one account may own. Default 7. |
| `MAX_OWNERS_PER_NET` | no | Owners one net may have. Default 5. |
| `PLAUSIBLE_DOMAIN` | no | Site domain reported to a Plausible instance. Empty means analytics is entirely off. |
| `PLAUSIBLE_SCRIPT_HOST` | no | Origin of a self-hosted Plausible. Ignored when `PLAUSIBLE_DOMAIN` is empty. |
| `PORT` | no | Port the binary listens on inside the container. Default 3000; must match the container side of the compose port mapping. |
| `POSTGRES_PASSWORD` | yes | Password the compose file hands the `postgres` service. Must match the password inside `DATABASE_URL`. |
| `PUBLIC_BASE_URL` | yes | The public origin emailed magic links point at. Must be `https://` except on loopback. |
| `SMTP_HOST` | no | SMTP relay host. Without a working relay nobody can sign in. |
| `SMTP_PASSWORD` | no | SMTP password. |
| `SMTP_PORT` | no | SMTP relay port. |
| `SMTP_USERNAME` | no | SMTP username. Empty selects an unauthenticated transport. |
| `TOKEN_RETENTION_DAYS` | no | Days dead sign-in tokens and sessions are kept before the hourly sweep deletes them. Default 30. |
<!-- env-table:end -->

The `CSP_POLICY` and `CSP_REPORT_ONLY` overrides are documented in the configuration reference
and left unset in both templates on purpose; the derived policy is the one to run.

### Compose-only values

- `IMAGE_REF` is the image repository `compose.prod.yml` pulls, without a tag. It defaults to
  `ghcr.io/nkbooth/netroll`; set it when you run an image you built and pushed yourself.
- `IMAGE_TAG` is interpolated by `compose.prod.yml` and is not read by the app. It defaults to
  `latest`; set it to a version tag for a pinned deploy. The GitHub workflow writes it as
  `<tag>@sha256:…`, so a rollback names the exact image even after a tag is re-pushed.
- `POSTGRES_PASSWORD` does double duty: the compose file passes it to the `postgres` service,
  and the app reads the same password inside `DATABASE_URL`.

## The reverse proxy

The proxy and the app split the response headers, and the split matters.

- **Transport headers belong to the proxy:** HSTS, `X-Content-Type-Options`, `Referrer-Policy`,
  `Permissions-Policy`, `X-Frame-Options`, and `X-Robots-Tag: noindex` on `/nets/t/*` so a
  permalink carrying its access token stays out of search indexes.
- **Content headers belong to the app.** It sends a Content Security Policy on every HTML
  response, derived from `PUBLIC_BASE_URL` and `PLAUSIBLE_SCRIPT_HOST`. A proxy that sets one
  too produces two headers, and a browser enforces their intersection, which breaks the app the
  first time the two disagree. Do not set a CSP at the proxy.

The app must be reachable only through the proxy. It trusts `X-Forwarded-For` and `X-Real-Ip`
as the client address, so a client that can reach the published port directly defeats every
rate limiter. Bind the published port to loopback when the proxy is on the same host, or
firewall it to the proxy's address when it is not.

`deploy/reverse-proxy/Caddyfile.example` is that split written down; the configuration
reference's "Content Security Policy" section covers the derived policy.

## Upgrading and rolling back

Set `IMAGE_TAG` to the release you want, then `podman compose -f compose.prod.yml pull` and
`up -d`. Roll back by editing `IMAGE_TAG` down and running the same two commands. The app
migrates its schema forward at boot and never backward, so read
[upgrade an instance](../docs/self-hosting/upgrade-an-instance.md) before rolling back across a
release that carried a migration.

## Deploying with the GitHub workflow

`.github/workflows/deploy.yml` builds the image, pushes it, and rolls it out on one host over
SSH. It deploys the public instance, and every value specific to that instance is something you
set, not something you edit out. Nothing in it is required to self-host; the steps above are
the whole of that.

### What it does

- **When it runs:** a published release, or a manual dispatch from the Actions tab with an
  optional tag. It never runs for a pull request, and a merge to `main` doesn't deploy.
- **`build`** (`contents: read`, `packages: write`): generates the cargo and npm SBOMs, builds
  and pushes `IMAGE_REF:<tag>`, and attaches the SBOMs to the pushed image. `latest` moves only
  for a plain `vX.Y.Z` that is not a prerelease, never for a dispatch on a branch, and `latest`
  itself is refused as a tag.
- **`deploy`** (`contents: read`, `packages: read`, environment `hero-production`): loads the
  host details and SSH key from 1Password, joins Tailscale if you configured it, renders
  `.env.next` from `deploy/.env.production.tpl` with `IMAGE_TAG` pinned to the pushed digest,
  copies it into the deploy directory with `compose.prod.yml` (as `compose.yml.next`) and
  `rollout.sh`, logs the host into GHCR with the run's own short-lived token, and runs
  `rollout.sh`.
- **`rollout.sh`** pulls the new image, keeps the current `.env` and `compose.yml` as
  `.previous`, moves the `.next` pair into place, runs `up -d`, and polls `/healthz` for two
  minutes. If the new release never answers `ok`, it puts the `.previous` pair back, runs
  `up -d` again, and polls that. It exits `0` when the new release is healthy, `1` when nothing
  changed (no `.next` files, or the pull failed), `2` when it rolled back to a healthy stack,
  `3` when the rollback is unhealthy too, and `4` when a first deploy failed with nothing to
  roll back to.
  It never touches the database. A rollback across a release that carried a migration fails its
  own health check by design, because the older binary refuses the newer schema; restore the
  dump as [upgrade an instance](../docs/self-hosting/upgrade-an-instance.md) describes.

The host, user and path live in the vault, not in repository variables: loaded secrets are
masked in the run log, variables are not, and a public repository's run logs are public.

### A fork is inert until you arm it

- GitHub disables Actions on a fork until you enable them.
- A fork inherits no secrets, no variables and no environment rules.
- Without `OP_SERVICE_ACCOUNT_TOKEN` the `deploy` job stops at its first 1Password step. The
  `op://` references in the repository are names, not values.

### Arm it for your own host

1. **Create a 1Password vault named `netroll-deploy`** holding these items. Item and field names
   are what the workflow and the template read, so keep them exactly.

    | Item | Fields | Read by |
    |---|---|---|
    | `deploy-target` | `host`, `user`, `path` | the workflow: the SSH host, the deploy user, and the deploy directory relative to that user's home |
    | `deploy-ssh` | `private-key`, `host-key` | the workflow: an SSH private key the deploy user accepts, and the host's full `known_hosts` line |
    | `tailscale` | `client-secret` | the workflow, only when `TS_OAUTH_CLIENT_ID` is set |
    | `database` | `url`, `password` | `.env`: `DATABASE_URL` (host `postgres`) and `POSTGRES_PASSWORD` |
    | `smtp` | `host`, `port`, `username`, `password` | `.env`: the `SMTP_*` values |
    | `kek` | `key` | `.env`: `KEK` |
    | `bot-mitigation` | `secret` | `.env`: `BOT_MITIGATION_SECRET` |
    | `instance` | `mail-from`, `public-base-url`, `admin-emails`, `plausible-domain`, `plausible-script-host`, `kofi-username` | `.env`: the values that identify your instance |

    For an optional integration you don't use — analytics, the Ko-fi link, `KEK`, bot
    mitigation — delete its line from your copy of `deploy/.env.production.tpl` rather than
    leaving the field empty.
2. **Create a service account that can read that one vault and nothing else:**
   `op service-account create netroll-deploy-ci --vault netroll-deploy:read_items`. A service
   account can't be re-scoped after it's created, so get this right the first time; a leaked
   token then reads deploy secrets and writes nothing.
3. **Create the environment `hero-production`** in the repository's settings and add the
   token as the environment secret `OP_SERVICE_ACCOUNT_TOKEN`, so only the `deploy` job can
   read it. On a public repository, give the environment a required reviewer: no secret is
   loaded until someone approves the run.
4. **Set the repository variable `IMAGE_REF`** to an image repository your workflow can push to,
   such as `ghcr.io/<owner>/netroll`. The default is the upstream repository, which a fork can't
   push to. If the host is reachable only over Tailscale, also set `TS_OAUTH_CLIENT_ID` to an
   OAuth client allowed to tag nodes `tag:ci`, and put its secret in the `tailscale` item.
5. **Prepare the host.** `rollout.sh` needs rootless Podman with `podman compose`, `curl`
   inside the image (it ships one), and a lingering systemd user session for the deploy user
   (`loginctl enable-linger <user>`) so containers and their health checks keep running with
   nobody logged in. The deploy user must accept the `deploy-ssh` key. Choose the deploy
   directory once: the compose project, and so the names of the data volumes, come from its
   basename, and moving it later starts the stack on empty volumes.
6. **Publish a release, or dispatch the workflow.** The first deploy has no `.env` to roll back
   to; `rollout.sh` says so if it fails.
7. **Optional: automatic releases.** `release-please.yml` opens release PRs and publishes the
   release when one merges, running as a GitHub App because the repository forbids Actions from
   creating pull requests. Create an App with repository permissions `contents`, `issues` and
   `pull-requests` set to write, install it on your repository only, and set the repository
   variable `RELEASE_APP_CLIENT_ID` and the repository secret `RELEASE_APP_PRIVATE_KEY`. Without
   it that workflow fails on every push to `main`, and you publish releases by hand.

## What the public instance does that this workflow doesn't

The instance at `netroll.n1cck.radio` adds three things around the same image and compose file:

- TLS termination and HSTS at a separate reverse proxy.
- Database backups taken and kept off the repository.
- A private network as the transport between the deploy workflow and the host.

Nothing in this repository depends on any of them.
