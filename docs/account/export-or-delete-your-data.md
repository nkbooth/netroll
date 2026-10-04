---
title: "Export or delete your data"
description: "Download your personal NetRoll data in a portable format, delete your account, and recover from a mistaken deletion within the 15-minute undelete window."
docType: "howto"
audience: ["participant", "operator"]
lastVerified: "2026-07-31"
---

# Export or delete your data

Take your data with you, or remove your account. Both are self-service.

## Prerequisites

- [A signed-in account](sign-in.md).

## Steps

### Step 1: Export your data

Request a data export from your profile. You receive your personal data in a portable format:

- Your profile and callsign
- Your check-in history
- Your favorites
- Delivery configurations on nets you own

The export contains your data only. Other users' data is excluded, including from sessions you
took part in.

### Step 2: Delete your account

Request deletion from your profile.

Your account moves to a pending-deletion state and stops being usable. It is not gone yet.

### Step 3: Undelete, if you didn't mean it

You have a **15-minute grace window**. Sign in again during it — request a fresh magic link and
follow it — and your account is fully restored.

### Step 4: Let deletion finalize

After the window elapses, a background job finalizes deletion of your account and its identity
and authentication data. The deletion is audit-logged.

## Verify

Export first, then delete. Open the export file and confirm it has what you expect before the
grace window runs out — after deletion finalizes, there's nothing left to export.

## What happens to nets you own

Deletion does not silently destroy other people's nets:

| Net | Result |
|-----|--------|
| Co-owned with someone else | The net survives under the remaining owners. Only your account leaves the owner set. |
| Solely owned by you | The net is archived — no longer listed, no longer live-runnable. It is not hard-deleted, so its past sessions stay attributable. |

If you want a club net to keep running after you leave, [add a co-owner](../nets/manage-owners.md)
before you delete.

## What stays behind

Sessions you took part in are historical records belonging to the nets that ran them, not to
your account. Check-ins you logged as an operator keep their attribution so past logs remain
accurate.

NetRoll stores minimal personal data by design, and the security audit log deliberately excludes
personal data — it records actor, action, target, and timestamp, with callsigns treated as
public and emails, tokens, and QRZ credentials excluded entirely.

## Related guides

- [Sign in](sign-in.md) — changing your email instead of deleting
- [Manage owners](../nets/manage-owners.md)
- [Connect QRZ](connect-qrz.md)
