---
title: "Connect QRZ"
description: "Store your QRZ callbook credentials in NetRoll so check-in lookups use your subscription, and understand the envelope encryption that keeps them unreadable."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-07-31"
---

# Connect QRZ

Store your QRZ XML credentials so check-in autofill queries QRZ under your own subscription.
Without them, NetRoll falls back to hamcall.dev, which covers US callsigns only.

## Prerequisites

- [A signed-in account](sign-in.md).
- A QRZ account with XML data access.
- An instance with an encryption key configured. Without one, the QRZ credential endpoints
  return HTTP 503 — see [`KEK`](../reference/configuration.md#envelope-encryption).

## Steps

### Step 1: Open your QRZ credentials

Go to **Profile** and find the QRZ credentials section.

### Step 2: Enter and save your credentials

Enter your QRZ callsign and password, and save.

The credentials are encrypted immediately with a per-record key, itself wrapped by an
instance-wide key injected at runtime. The plaintext is never persisted.

### Step 3: Confirm they're in use

Log a check-in for a callsign you know QRZ has data for. Name and location autofill, and stay
editable.

## Verify

Reopen the QRZ credentials section. It shows that credentials are set, and does not show the
password — the field is write-only from the UI, and the stored password is never returned to the
client.

## How lookups use them

Lookup is provider-abstracted and best-effort:

1. If you have QRZ credentials stored, NetRoll queries QRZ XML with them.
2. If QRZ returns nothing, fails, or you have no credentials stored, it falls back to
   hamcall.dev (US callsigns only).
3. If everything fails, it returns nothing and you type the details yourself.

A lookup failure never blocks a check-in. Results are cached and lookups are rate-limited, so a
busy net doesn't hammer the provider.

Values from your NetRoll profile always win over looked-up values, and everything autofilled
stays editable.

## How your credentials are protected

**Never readable by an admin.** Each credential record is sealed with its own data key, wrapped
by an instance key that lives only in the running process — never in Postgres and never in
backups. An instance running without that key cannot decrypt stored credentials at all.

**Never logged.** Credentials do not appear in logs or traces.

**Only ever outbound over the safe egress client.** QRZ requests go through the same
SSRF-guarded client as webhooks: HTTPS-only, pinned resolved IP, bounded redirects, timeouts,
and a response size cap.

## Related guides

- [Log check-ins](../live-sessions/log-check-ins.md) — where the autofill shows up
- [Reserve a callsign](reserve-a-callsign.md)
- [Configuration reference](../reference/configuration.md) — the `KEK` an instance needs for
  this feature
