# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Production .env template for NetRoll — rendered by `op inject` in CI (see
# .github/workflows/deploy.yml) and shipped to the deploy host as .env.next, which
# deploy/rollout.sh swaps into place.
#
# The 1Password secret references resolve against the vault "netroll-deploy" using
# the service account whose token lives in the OP_SERVICE_ACCOUNT_TOKEN secret of
# the deploy job's environment. deploy/README.md lists the items and fields. This
# file is safe to commit: it holds references and policy, never a secret or a value
# that identifies an instance.
#
# CI appends IMAGE_REF and IMAGE_TAG at deploy time to pin the image
# (deploy/compose.prod.yml references ${IMAGE_REF}:${IMAGE_TAG}).
#
# An optional value your instance doesn't use is DELETED from your copy, never
# pointed at an empty field. Every var the app reads is either set here or
# documented as deliberately left to its default (STATIC_DIR, AVATAR_DIR). NOTE: op
# inject resolves secret references even in comments, so disabled refs must not
# use the real syntax.

# ---- Core ----
PORT=3000

# ---- Database ----
# compose.prod.yml interpolates POSTGRES_PASSWORD for the postgres service;
# the app consumes DATABASE_URL (whole URL in the vault, host `postgres`).
DATABASE_URL=op://netroll-deploy/database/url
POSTGRES_PASSWORD=op://netroll-deploy/database/password

# ---- SMTP (magic-link email delivery) ----
SMTP_HOST=op://netroll-deploy/smtp/host
SMTP_PORT=op://netroll-deploy/smtp/port
SMTP_USERNAME=op://netroll-deploy/smtp/username
SMTP_PASSWORD=op://netroll-deploy/smtp/password
# REQUIRED, non-secret: the From address on every outgoing email. The app
# refuses to boot while this is unset (no fallback sender). Must be an address
# the SMTP relay is authorised to send as, on a domain with SPF/DKIM for it.
MAIL_FROM=op://netroll-deploy/instance/mail-from

# ---- Public URL ----
# REQUIRED, non-secret: emailed magic links point here. The app refuses to
# boot while this is empty or unset (no silent dev-origin fallback), and
# refuses anything but an https:// origin — plain http is accepted only for
# loopback hosts, which a production value never is.
PUBLIC_BASE_URL=op://netroll-deploy/instance/public-base-url

# ---- Envelope encryption ----
# Instance key-encryption key for stored QRZ credentials — injected at runtime,
# never written to Postgres or backups, so it must NOT be rotated casually:
# every credential sealed under the old KEK becomes undecryptable. Base64 32
# bytes (`openssl rand -base64 32`). Absent ⇒ the app still boots and QRZ
# endpoints return 503; present-but-malformed is a hard boot error.
KEK=op://netroll-deploy/kek/key

# ---- Resource caps ----
# Per-account resource limits. Plain integers, NOT secrets. Both are set here
# rather than left unset so this file states the instance's policy outright — an
# absent value inherits a code default, which means the deployed limit lives in a
# source constant instead of in the deploy record, and changes without a deploy.
#
# Max active (non-archived) nets one account may own. CAUTION: the net-creation
# rate limiter's burst is a fixed 12 (rate_limit.rs::NET_CREATION_BURST) and is
# NOT env-configurable — raise this to 12 or above and a user filling their whole
# quota in one sitting can be throttled before reaching it. The app logs a boot
# warning if that happens; raising the burst requires a source change.
MAX_NETS_PER_USER=7
# Max owners per net.
MAX_OWNERS_PER_NET=5

# ---- Aggregate magic-link cap ----
# Instance-wide ceiling on outbound magic-link email, in sends per hour. Plain
# integer, NOT a secret. Unset ⇒ 60/hour; a set-but-invalid value (0, negative,
# non-numeric) aborts boot rather than silently falling back.
#
# Separate from — never a replacement for — the fixed per-address quota (3 per
# address per 15 min): that one is independent per address, so a harvested list
# would otherwise drive unbounded mail through this relay. A blocklisted sending
# domain means no magic link reaches anyone, i.e. nobody can sign in at all.
#
# EVERY request draws on this budget. Capped requests get the ordinary 202 with
# no mail, so the cap cannot be used to probe which addresses have accounts.
MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR=60

# Reserve budget drawn on ONLY once the cap above is spent, and ONLY by addresses
# that signed in here within the last 30 days — so returning users still
# authenticate during a campaign (that proven-address set is in-memory and
# empties on restart). Unset ⇒ 120/hour; set-but-invalid aborts boot. Total
# outbound magic-link mail is bounded by the two together.
#
# NOT A GUARANTEE: it is a bounded budget, so it can be drained. ~10 addresses
# that have each signed in here once, each requesting 3 links per 15 min (within
# the per-address quota), consume 120/hour — the whole default reserve — after
# which a genuine returning user is dropped too. Raising this raises the effort
# to drain it and the total outbound ceiling by the same amount.
#
# SIZING: each number is a bucket that starts FULL and refills at that rate, so a
# pool of N admits N at once plus up to another N over the next hour — up to 2N
# in the first hour after boot, then N/hour sustained. Defaults therefore admit
# UP TO 360 SENDS IN A FRESH-BOOT HOUR (120 general + 240 reserve) against a
# sustained 180/hour, and a crash-looping process resets both buckets every boot.
# Size your relay's reputation against 360, not 180.
MAGIC_LINK_RESERVE_SENDS_PER_HOUR=120

# ---- Retention ----
# Days that dead magic-link tokens, sessions and email-change tokens are kept
# before a background sweep deletes them. Plain integer, NOT a secret. Unset ⇒
# 30; a set-but-invalid value (0, negative, non-numeric) aborts boot rather than
# silently falling back — 0 would collapse the cutoff onto "now" and leave no
# retained history to correlate a sign-in against the audit log.
#
# Set here rather than left to its default because this instance carries real
# third-party clubs' nets and those three tables had no ceiling and no mechanism
# that would ever remove a row: the account-deletion cascade fires only on
# finalization, so it never runs for a live account and never reaches the
# email-keyed magic_link_tokens at all.
#
# ONE window for all three tables. Deletes tokens whose expiry is past the
# window, and sessions whose absolute 30-day cap OR whose revocation is past it.
# Idle sessions are NOT pruned — idle expiry is a per-request judgement against a
# different window. No signed-in user is signed out by this sweep.
#
# BEST-EFFORT: runs at startup then hourly, does not batch, is not awaited at
# shutdown, and a failed tick simply waits for the next hour. None of the three
# filtered columns is indexed either — deliberate, to keep write amplification
# off the sign-in path — so each tick is three sequential scans, hourly, forever.
#
# In steady state nothing observes a row between dying and being removed, so a
# missed sweep costs only delay. The FIRST sweep on this instance is different,
# because those limits compose: these tables have never been pruned, so it is the
# largest single DELETE the system will ever issue, it starts at boot, and one
# unbatched unawaited statement means a restart mid-sweep rolls back ALL of it.
# That costs a full rollback plus dead-tuple churn, re-incurred on every boot
# until one sweep completes. It will complete (no statement timeout is set) — let
# the first boot after this lands run through rather than restarting into it.
TOKEN_RETENTION_DAYS=30

# ---- Bot mitigation ----
# HMAC secret for the honeypot + timing form tokens on signup and net creation.
# Its PRESENCE is the on/off switch: absent ⇒ mitigation DISABLED. Must be at
# least 16 bytes, since GET /api/form-tokens hands out token/signature pairs a
# short key is brute-forceable from. `openssl rand -base64 32`.
BOT_MITIGATION_SECRET=op://netroll-deploy/bot-mitigation/secret

# ---- Platform admins ----
# Comma-separated admin emails, matched case-insensitively against the account's
# normalized stored email — so this only grants anything once an account has
# registered under it. Not a secret, but it identifies the instance's people, so
# it lives in the vault. Unset ⇒ the bounded admin surface (abuse reports,
# disable/re-enable accounts) is unreachable.
ADMIN_ACCOUNT_EMAILS=op://netroll-deploy/instance/admin-emails

# ---- Analytics (self-hosted Plausible; cookieless) ----
# Unset ⇒ analytics entirely off: the tracker chunk is never downloaded and
# nothing is requested from any external host. PLAUSIBLE_DOMAIN is the site this
# instance reports as and MUST match a site registered in the Plausible instance;
# PLAUSIBLE_SCRIPT_HOST is that instance's origin (events go to /api/event there)
# and is ignored without a domain to report as.
PLAUSIBLE_DOMAIN=op://netroll-deploy/instance/plausible-domain
PLAUSIBLE_SCRIPT_HOST=op://netroll-deploy/instance/plausible-script-host

# ---- Content Security Policy ----
# CSP_POLICY and CSP_REPORT_ONLY are DELIBERATELY UNSET: the derived default is
# the policy of record. Its two instance-specific origins are derived — the
# WebSocket origin from PUBLIC_BASE_URL, the analytics origin from
# PLAUSIBLE_SCRIPT_HOST — so connect-src follows those two variables and a change
# to either needs no proxy edit and no entry here. Setting CSP_POLICY would
# replace the whole policy and forfeit that derivation; CSP_REPORT_ONLY=true would
# stop enforcing it, frame-ancestors included.

# ---- Footer support link ----
# The Ko-fi USERNAME, not a URL — the app builds https://ko-fi.com/<username>
# and renders a plain link (no third-party widget). Unset ⇒ no link at all.
KOFI_USERNAME=op://netroll-deploy/instance/kofi-username

# ---- Uploaded avatars ----
# AVATAR_DIR is baked into the image as /app/data/avatars and backed by the
# netroll-avatars volume in compose.prod.yml; override here only to move it.
