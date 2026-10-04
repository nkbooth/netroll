---
title: "Sign in"
description: "Sign in to an existing NetRoll account with a single-use email magic link, sign out to revoke the session, and move your account to a new identifying email address."
docType: "howto"
audience: ["participant", "operator"]
lastVerified: "2026-07-31"
---

# Sign in

Sign in to an account you already have, and manage the session it creates. NetRoll has no
passwords — every sign-in is a single-use link emailed to your address.

Setting one up for the first time is covered in
[create an account](../getting-started/create-an-account.md). The same flow does both, so
there's no separate sign-up form to find.

## Prerequisites

- An email address you can read now.

## Steps

### Step 1: Request a link

Go to **Sign in**, enter your email address, and request a link.

### Step 2: Open the link

Open the email and follow the link. The link is single-use and short-lived.

Consuming it marks your email verified and establishes a server-side session held in a Secure,
HttpOnly, SameSite cookie. The link cannot be used a second time.

### Step 3: Accept the terms

On first sign-in, accept the terms. Consent is recorded with a timestamp, and gated actions stay
blocked until it is. You won't be asked again on later sign-ins.

### Step 4: Sign out

Sign out to revoke the session server-side. Sessions are revocable — signing out invalidates the
session rather than only clearing a cookie in your browser.

## Verify

Reload the app. You stay signed in, and your profile shows your email as verified.

## Change your identifying email

Request the change from your profile with the new address.

NetRoll emails a link to the **new** address, and the change takes effect only once you open it.
Until then your old email remains in effect. An address already used by another account is
rejected.

When the change completes, existing sessions are invalidated and you sign in again — from the
new address.

## Troubleshooting

**The link says it's expired or already used.** Links are single-use and short-lived. Request a
new one.

**Requests are being refused with a rate-limit error.** Magic-link requests for one email are
throttled. Wait for the window to reset, then request again.

**Nothing arrived.** Check spam. If you're on a self-hosted instance, the instance's SMTP
settings may be wrong or its sender address unroutable — see
[troubleshooting sign-in](../troubleshooting/signing-in.md).

## Related guides

- [Reserve a callsign](reserve-a-callsign.md)
- [Export or delete your data](export-or-delete-your-data.md)
- [Troubleshooting sign-in](../troubleshooting/signing-in.md)
