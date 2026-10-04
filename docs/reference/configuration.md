---
title: "Configuration"
description: "Every environment variable NetRoll reads, its type, default, and whether an invalid value is a hard boot error or degrades the feature that uses it."
docType: "reference"
audience: ["self-hoster", "contributor"]
lastVerified: "2026-09-13"
---

# Configuration

NetRoll is configured entirely through environment variables. This page lists every variable the
app reads.

Two behaviours recur throughout and are worth knowing before you read the tables:

- **Unset optional variables degrade cleanly.** No encryption key means QRZ endpoints return
  HTTP 503; no admin emails means the admin surface is unreachable; no analytics domain means no
  tracker is ever downloaded.
- **Set-but-invalid values are hard boot errors.** A value that looks configured but silently
  does nothing is treated as worse than a refusal to start.

## Core

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `PORT` | integer | No | `3000` | TCP listen port. Non-numeric, out of range, or `0` is a boot error. |
| `STATIC_DIR` | path | No | `frontend/dist` | Directory holding the built SPA. Baked into the container image. Set-but-empty is a boot error, as is a path with no `index.html`. |
| `DATABASE_URL` | string | **Yes** | — | Postgres connection string. Unset or empty is a boot error — there is no fallback. |
| `PUBLIC_BASE_URL` | URL | **Yes** | — | Base URL emailed magic links point at; set it to the public origin your reverse proxy serves. Unset or empty is a boot error — there is no dev-server fallback. Must be an `https://` origin: any other scheme, an empty host, or plain `http://` on anything but a loopback host (`localhost`, `127.0.0.1`, `[::1]`) is a boot error. A trailing slash is stripped. |

## Email

Sign-in is a magic link, so an instance without working SMTP is one nobody can log in to, even
though the app boots.

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `SMTP_HOST` | hostname | Yes, in practice | — | SMTP relay host. |
| `SMTP_PORT` | integer | Yes, in practice | — | SMTP relay port. Non-numeric or out of range is a boot error. |
| `SMTP_USERNAME` | string | No | empty | Empty selects the unauthenticated transport. Set it only when the relay requires AUTH. |
| `SMTP_PASSWORD` | string | No | empty | Paired with `SMTP_USERNAME`. |
| `MAIL_FROM` | string | **Yes** | — | The From address on every outgoing email, bare or as `Display Name <user@domain>`. Unset is a boot error — there is no fallback sender. Must be an address the relay is authorized to send as. |

## Envelope encryption

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `KEK` | base64 | No | none | Instance key-encryption key for stored QRZ credentials. Base64-encoded 32 bytes for AES-256. Unset means the app boots normally and QRZ credential endpoints return HTTP 503. Present but malformed — bad base64 or wrong length — is a boot error. |

Mint one with:

```bash
openssl rand -base64 32
```

The key is injected at runtime and never written to Postgres or backups. **Do not rotate it
casually**: every credential sealed under the old key becomes permanently undecryptable.

## Resource caps

Plain integers, not secrets. Unset uses the documented default; a set-but-invalid value — zero,
negative, or non-numeric — is a boot error rather than a silent fallback.

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `MAX_NETS_PER_USER` | integer | No | `7` | Maximum active, non-archived nets one account may own. |
| `MAX_OWNERS_PER_NET` | integer | No | `5` | Maximum owners on one net. |

**Caution on raising `MAX_NETS_PER_USER`.** The net-creation rate limiter has a fixed burst of
12, which is not configurable by environment. Set the cap to 12 or higher and a user filling
their whole quota in one sitting may be throttled before reaching it. The app logs a boot-time
warning when the two are in that relationship; raising the burst itself requires a source change.

## Aggregate magic-link cap

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR` | integer | No | `60` | Instance-wide ceiling on outbound magic-link email, in sustained sends per hour across all addresses combined. The first hour after a restart allows up to twice this — see "What these numbers actually cap". Zero, negative, or non-numeric is a boot error. |
| `MAGIC_LINK_RESERVE_SENDS_PER_HOUR` | integer | No | `120` | Second-tier budget, in sustained sends per hour, drawn on only once the ceiling above is spent, and only for addresses that signed in on this instance recently (or, on the authenticated email-change path, for any signed-in caller). Same first-hour doubling. Zero, negative, or non-numeric is a boot error. |

This is **independent of, and not a replacement for, the per-address quota** of 3 magic-link
requests per address per 15 minutes. That quota is fixed in source and has its own budget for
every address, so cycling a harvested address list would otherwise drive unbounded mail through
your SMTP relay. Both limits are in force, and the per-address one is evaluated first — a
single-address abuser still gets an honest `429`.

The two budgets work as one two-tier cap. **Every** magic-link request draws on
`MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR` first, whoever it is for. Only when that budget is spent
may an address that completed a sign-in on this instance within the last 30 days fall back to
`MAGIC_LINK_RESERVE_SENDS_PER_HOUR`. Everything else is dropped.

That reserve is what keeps an abuse campaign against unknown addresses from locking your own
users out of the only sign-in path the app has — but it is a budget, not a guarantee, and it can
itself be drained. See "What the reserve does and does not promise" below. The set of
recently-signed-in addresses is held in memory and is emptied by a restart.

Requests over both budgets get the same `202 Accepted`, with no mail sent and no token issued,
that a successful request gets. This is deliberate: a distinguishable response would let a caller
probe which addresses have accounts.

**The authenticated email-change path spends these same two budgets.** `POST
/api/accounts/me/email-change` mails a confirmation link to the new address, and that send draws
on `MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR` first and `MAGIC_LINK_RESERVE_SENDS_PER_HOUR` second, the
same order a magic-link request uses — so confirmation-link sends across both endpoints stay
bounded by general + reserve, whichever endpoint drove them. (The one courtesy notice mailed to
the *old* address when a change is confirmed is not drawn from either budget; it is bounded by the
confirmations, at most one per admitted request.) Who may fall back to the reserve differs: any
signed-in, consented caller qualifies, not the recently-signed-in-address set above. The caller's
live session is the proof of control; the address being mailed is not one the caller has proven
control of, and gating on whether *it* has signed in here would let the response reveal that.
A capped email-change request is the same silent `202` — no mail, no token, and the
"is that address already taken" check is skipped too, so the cap cannot be used to probe accounts.

So that one account cannot drain the shared budget by itself, the email-change path also carries
a **per-account quota**, fixed in source: a burst of 5 requests per account, then one every 10
minutes. It is checked before the shared budgets, so a throttled account spends none of them, and
it answers an honest `429` with `Retry-After` — the key is the caller's own account, so the answer
says nothing about the target address.

### What these numbers actually cap

Each budget is a token bucket that **starts full** and then refills at its configured rate. A
pool of N therefore admits N sends immediately, plus up to another N replenished over the
following hour — **up to 2N in the first hour after the process starts**, settling to N per hour
sustained after that.

| Budget | Sustained | First hour after boot |
|--------|-----------|------------------------|
| `MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR` (default 60) | 60/hour | up to 120 |
| `MAGIC_LINK_RESERVE_SENDS_PER_HOUR` (default 120) | 120/hour | up to 240 |
| **Total** | **180/hour** | **up to 360** |

A crash-looping process resets both buckets on every boot, so the burst figure is not a one-time
startup allowance — it is repeatable. **If you are sizing what your SMTP relay's sending
reputation has to survive, use the burst number (360 on the defaults), not the sustained one.**
Both figures scale linearly with the two variables.

The default of 60 per hour sits far above a small instance's real signup and sign-in volume while
bounding an email-bombing campaign to a volume a relay's sending reputation survives. Raise both
if you run a large instance.

### What the reserve does and does not promise

The reserve makes a returning user's sign-in survive a campaign against *unknown* addresses,
which is what it exists for. It does not make sign-in unconditionally available, because the
reserve is a finite budget and a determined attacker can drain it too.

The concrete shape: an attacker who controls a catch-all domain signs in once from each of about
ten addresses (a one-time cost of ten cells from the general pool), which puts all ten into the
recently-signed-in set. Each of those addresses may then request a link 3 times per 15 minutes
without tripping the per-address quota — 120 requests per hour across the ten, exactly the
reserve's default replenish rate. The reserve then sits at zero, and a genuine returning user
gets the silent `202`: the outcome the reserve exists to prevent.

Raising `MAGIC_LINK_RESERVE_SENDS_PER_HOUR` raises how much draining it takes, and it raises the
outbound ceiling by the same amount. The two move together; there is no setting that buys one
without the other. This is a deliberate trade against the alternative — an unlimited exemption
for recently-signed-in addresses, which could not be drained but left total outbound mail
unbounded once an attacker had established exemptions.

## Retention

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `TOKEN_RETENTION_DAYS` | integer | No | `30` | How long dead magic-link tokens, sessions, and email-change tokens are kept before a background sweep deletes them. Zero, negative, or non-numeric is a boot error. |

One window covers all three tables. A sweep runs at startup and then hourly, deleting:

- magic-link tokens and email-change tokens whose expiry is older than the window,
- sessions whose absolute 30-day cap, or whose revocation, is older than the window.

Sessions are **not** pruned on idle time. A session you have not used in months but whose
absolute cap has not been reached is left alone — idle expiry is decided per request, and
deleting the row would tie its lifetime to a policy the sweep does not own. Nothing this sweep
deletes can sign a signed-in user out.

The sweep is best-effort. It does not batch its deletes, it is not waited for at shutdown, and a
tick that fails is not retried until the next hour. None of the three columns it filters on is
indexed, deliberately — an index would add write amplification to the sign-in path — so every
tick is three sequential scans, hourly, for as long as the process runs. At any realistic
instance size that cost is negligible, and it is what keeps the tables bounded in the first
place.

In steady state, losing a sweep costs nothing but a delay: nothing observes a row between the
moment it dies and the moment it is removed. The **first** sweep is the exception, because those
limits compose. On tables that have never been pruned it is the largest single `DELETE` the
system will ever issue; it starts at boot rather than an hour in; and because it is one
unbatched statement that nothing waits for, a restart in the middle of it rolls back the whole
thing rather than part of it. An interrupted first sweep therefore costs a full rollback plus
the dead tuples it churned, re-incurred on every boot until one sweep runs to completion. It
will complete — no statement timeout is configured — so on a large backlog, let the first boot
after upgrading finish rather than restarting into it.

What the sweep cannot do is get in a signed-in user's way. Nothing foreign-keys to these three
tables, and it only ever selects rows already past the retention window — rows no live request
can be holding — so its row locks cannot collide with session refresh on an authenticated
request or with consuming a magic link. The worst case is one pooled connection held for the
duration and a burst of write-ahead log.

Lower the window if you want tighter data minimisation; raise it if you want more history to
correlate a sign-in against the audit log. Existing rows already past a newly-lowered window go
on the next sweep.

## Bot mitigation

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `BOT_MITIGATION_SECRET` | string | No | none | HMAC signing secret for the honeypot and timing form tokens on signup and net creation. Its presence is the on/off switch: unset or empty disables mitigation. Must be at least 16 bytes — present but shorter is a boot error. |

The minimum length is enforced because `GET /api/form-tokens` hands out token and signature
pairs publicly. A short key is brute-forceable from those pairs, which would defeat mitigation
while appearing configured.

Mint one with `openssl rand -base64 32`. Mitigation is self-hosted — there is no external CAPTCHA
vendor.

## Uploaded avatars

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `AVATAR_DIR` | path | No | `data/avatars` | Directory uploaded avatars are written to and served from, as `/avatars/<account-id>.<ext>`. Set-but-empty is a boot error, as is an unwritable path. |

The directory is created at boot and probed for writability, so a misconfigured volume fails at
startup rather than on a user's first upload.

**Mount a volume here in production.** Avatars are the only user data outside Postgres, and
without a volume they're lost whenever the container is replaced.

Uploads are capped at 1 MB and accepted only when the file's own magic bytes say PNG, JPEG,
WebP, or GIF. The filename and `Content-Type` are never trusted.

## Platform admins

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `ADMIN_ACCOUNT_EMAILS` | comma-separated emails | No | none | Accounts granted the bounded admin surface. Matched case-insensitively against the account's normalized stored email. |

Unset or empty means no admins are configured — the app boots and the admin surface is
unreachable. A list containing a blank or non-address entry, such as one left by a trailing
comma, is a boot error rather than a silent partial parse.

The listed address grants nothing until an account registers under it. Platform admin is a
separate axis from [per-net roles](roles-and-permissions.md).

```bash
ADMIN_ACCOUNT_EMAILS=admin@example.com,ops@example.com
```

## Analytics

Off by default. See [enable analytics](../self-hosting/enable-analytics.md).

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `PLAUSIBLE_DOMAIN` | domain | No | none | The site domain this instance reports as. Must match a site registered in the Plausible instance, or events are rejected. Unset or empty means no analytics: the tracker chunk is never downloaded and nothing is requested from any external host. |
| `PLAUSIBLE_SCRIPT_HOST` | origin | No | Plausible Cloud | Origin of a self-hosted Plausible. Events are posted to `<origin>/api/event`. Ignored entirely when `PLAUSIBLE_DOMAIN` is unset. |

The tracker ships in NetRoll's own bundle, so no script is fetched from this host.

## Content Security Policy

The app sends its own `Content-Security-Policy` on every response whose `Content-Type` is
`text/html` — the app shell, every client-side route, and the documentation under `/docs`.
Responses of any other type — API JSON, the `/healthz` probe, JavaScript, CSS, images, avatars —
carry none. The header follows the content type, not the path: a request for a path no API route
matches, such as `GET /api/typo`, falls through to the SPA shell, which is HTML, and so returns
`200` with the header. Your reverse proxy must **not** set a CSP: two CSP headers on one response are both
enforced, and the intersection is a policy nobody tested.

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `CSP_POLICY` | string | No | derived | Replaces the whole policy verbatim, trimmed — no merging with the default. Set-but-empty is a boot error, as is a value containing a carriage return, a line feed, or any byte outside visible ASCII. |
| `CSP_REPORT_ONLY` | `true` / `false` | No | `false` | `true` sends the same policy as `Content-Security-Policy-Report-Only`: the browser logs violations to its console and enforces nothing. Case-insensitive, trimmed. Anything other than `true` or `false` is a boot error. |

The default policy, for `PUBLIC_BASE_URL=https://netroll.example` with analytics off, is:

```
default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; style-src-elem 'self'; style-src-attr 'unsafe-inline'; img-src 'self' data: https:; font-src 'self'; connect-src 'self' wss://netroll.example; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'
```

Directive by directive:

- **Scripts** come from the app's own origin only. The built shell has no inline script, so there
  is no hash, no nonce, and no `'unsafe-inline'`.
- **Styles are not origin-only.** `<style>` elements and stylesheets come from the app's origin
  only (`style-src-elem 'self'`), but inline `style=""` attributes are allowed
  (`style-src-attr 'unsafe-inline'`) because React sets them throughout the UI. The split is
  deliberate: an attribute cannot carry a selector, so it cannot do the attribute-selector
  exfiltration an injected `<style>` element can. The plain `style-src 'self' 'unsafe-inline'` is
  the fallback for browsers that predate the CSP level 3 elem/attr split; on those, injected
  `<style>` elements are not refused.
- **Images** come from the app's origin, `data:` URIs (Material for MkDocs embeds its icons in
  CSS under `/docs`), and any `https:` host, because an avatar may point at any HTTPS URL and
  Gravatar is one.
- **Fonts** come from the app's origin only.
- **`connect-src`** names the app's origin, the WebSocket origin derived from `PUBLIC_BASE_URL`
  (`wss://` for `https://`, `ws://` for a loopback `http://`; host and port kept, path dropped),
  and — only when `PLAUSIBLE_DOMAIN` is set — the analytics origin (`PLAUSIBLE_SCRIPT_HOST`, or
  Plausible's cloud host when that is unset). Change those two variables and `connect-src`
  follows; nothing else needs editing.
- **Plugins, framing and `<base>`** are refused outright (`object-src 'none'`,
  `frame-ancestors 'none'`, `base-uri 'none'`), and forms may submit to the app's origin only.

Use `CSP_REPORT_ONLY=true` to watch a browser console for violations during a rollout, then
remove it. There is no reporting endpoint. Report-only enforces nothing, `frame-ancestors 'none'`
included — and the app sets no `X-Frame-Options` of its own — so while it is on, the page is
frameable unless your proxy sets `X-Frame-Options: DENY` (the example Caddyfile does). Keep the
window short.

## Donations

| Variable | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `KOFI_USERNAME` | string | No | none | Ko-fi username behind the footer's support link. The app builds `https://ko-fi.com/<username>`. Unset or empty renders no link. |

A plain link only — no third-party widget, no overlay script, nothing that can track a visitor.

## Constraints

- Every variable is read at boot. Changing one requires restarting the app.
- The container side of your port mapping must equal `PORT`, or the host port forwards to
  nothing.
- Secrets are supplied as literal values by self-hosters. The `op://` references in
  `.env.template` and `deploy/.env.production.tpl` are 1Password bindings for the hero instance
  and are not a required mechanism.

## See also

- [Self-hosting quickstart](../self-hosting/quickstart.md)
- [Deployment model](../self-hosting/deployment-model.md)
- [Troubleshooting self-hosting](../troubleshooting/self-hosting.md)
