---
title: "Reserve a callsign"
description: "Reserve your callsign on NetRoll to unlock self-check-in and net ownership, and change it later without breaking past logs."
docType: "howto"
audience: ["participant", "operator"]
lastVerified: "2026-07-31"
---

# Reserve a callsign

Claim your callsign on the instance. It's the identifier the roster shows, and it gates
self-check-in and net ownership.

## Prerequisites

- [A signed-in account with a verified email](sign-in.md).

## Steps

### Step 1: Open your profile

Go to **Profile**.

### Step 2: Enter your callsign

Enter your callsign and save.

NetRoll validates international callsign formats and accepts portable suffixes — `/P` and `/M`
are normalized. A malformed callsign is rejected with a message saying what's wrong.

A callsign already held by another account is rejected as non-unique.

### Step 3: Fill in the rest of your profile

While you're here, set your display name, location, and Maidenhead grid. An invalid grid string
is rejected with a specific message.

Your avatar defaults to Gravatar derived from your account email. Upload one to override it.

## Verify

Open a live net and choose **Check in**. The action is available rather than routing you to
callsign setup.

## Changing your callsign

Enter a new valid, unused callsign. NetRoll reserves the new value and frees the old one for
reuse, both inside one transaction — the uniqueness rule always holds, and there's no window
where you hold neither.

## What a reserved callsign is and isn't

**Self-asserted.** NetRoll stores your callsign as an unverified display identifier. It does not
check license records. Reserving a callsign proves you got there first on this instance, nothing
more.

**Not required to watch or favorite.** Browsing, filtering, favoriting, and viewing live
sessions all work without one. Self-check-in and net ownership are the two things gated on
holding one.

**Why your profile beats a callbook.** When staff log a check-in for you, your NetRoll profile
values override anything the callbook lookup returns. Keeping your profile current is how you
control how you appear in other people's logs.

## Related guides

- [Sign in](sign-in.md)
- [Join a net](../getting-started/join-a-net.md)
- [Create a net](../nets/create-a-net.md)
- [Connect QRZ](connect-qrz.md)
