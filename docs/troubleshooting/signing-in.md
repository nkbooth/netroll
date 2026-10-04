---
title: "Troubleshoot signing in"
description: "Fix NetRoll magic-link problems: emails that never arrive, links that report as expired or used, rate limits, and email changes that don't take effect."
docType: "troubleshooting"
audience: ["participant", "operator"]
lastVerified: "2026-09-14"
---

# Troubleshoot signing in

NetRoll signs you in with a single-use emailed link and has no passwords, so nearly every
sign-in problem is an email problem. Fixes are grouped by what you see.

## No email arrives

**Symptoms:** You requested a link and nothing shows up, with no error on screen.

**Cause:** Requesting a link always reports success — the app doesn't reveal whether an address
has an account. So a silent non-arrival is either delivery or configuration.

**Fix:**

1. Check spam and any quarantine.
2. Confirm you typed the address correctly, then request again.
3. If you're on a self-hosted instance, check the instance's SMTP settings and sender domain.
   See [troubleshooting self-hosting](self-hosting.md).
4. In local development, look in Mailpit at `http://localhost:8025` rather than a real inbox.

**Verify:** The email arrives and the link signs you in.

---

## The link says it's expired or already used

**Symptoms:** Following the link gives a message that it's no longer valid.

**Cause:** Magic links are single-use and short-lived. Opening one twice, or opening an old one
after requesting a newer one, both hit this.

**Fix:**

1. Request a fresh link.
2. Open the newest email — earlier links stop being useful once you've requested another.
3. Open it in the same browser you requested it from.

**Verify:** The new link signs you in and you stay signed in after a reload.

---

## Requests are rejected with a rate-limit error

**Symptoms:** Requesting a link returns HTTP 429 or a "too many requests" message.

**Cause:** Magic-link requests for one email address are rate-limited to blunt abuse.

**Fix:**

1. Stop requesting. Each attempt keeps the window busy.
2. Wait for the window to reset.
3. Check your inbox in the meantime — an earlier link may already be there and still valid.

**Verify:** A later request succeeds and the email arrives.

---

## You're signed in but actions are blocked

**Symptoms:** You can browse, but creating a net or checking in is refused or routes you
elsewhere.

**Cause:** Three different gates look similar from the outside: consent not recorded, no
callsign reserved, or an account cap reached.

**Fix:**

1. Accept the terms if you're prompted. Gated actions stay blocked until consent is recorded.
2. [Reserve a callsign](../account/reserve-a-callsign.md). Self-check-in and net ownership both
   require one.
3. If net creation specifically is refused, you may be at the instance's cap on active nets —
   the default is 7. Archive a net you no longer run.

**Verify:** The action you wanted is available.

---

## An email change didn't take effect

**Symptoms:** You requested a new identifying email and your account still shows the old one.

**Cause:** The change requires you to prove control of the **new** address. Until you open the
link sent there, the old email remains in effect.

**Fix:**

1. Check the inbox of the **new** address, including spam.
2. Open the link there.
3. Sign in again — completing the change invalidates existing sessions on purpose.

If the change was rejected outright, the new address is already used by another account. Email
identity is unique per instance.

**Verify:** Your profile shows the new address, and a fresh magic link sent to it signs you in.

---

## Get help

File an issue at
[github.com/nkbooth/netroll/issues](https://github.com/nkbooth/netroll/issues)
with the instance you're using and what you saw.

If you're the operator of a self-hosted instance, check the app logs first — mail failures are
logged, and the credentials themselves never are.

## Related pages

- [Sign in](../account/sign-in.md)
- [Reserve a callsign](../account/reserve-a-callsign.md)
- [Troubleshoot self-hosting](self-hosting.md)
