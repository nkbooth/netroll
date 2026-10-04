---
title: "Create an account"
description: "Create your NetRoll account: request a magic link, verify your email, accept the terms, and reserve your callsign. Everything else in NetRoll starts here."
docType: "quickstart"
audience: ["participant", "operator"]
lastVerified: "2026-08-01"
---

# Create an account

Get a working NetRoll account. This is the first thing to do whether you plan to run nets or
just check in to them — both paths start here.

**Time:** ~3 minutes

## Do you actually need one?

Reading is open to everyone. You only need an account for the things that write to a net.

| You want to | Account | Callsign |
|-------------|:-------:|:--------:|
| Browse and filter nets | — | — |
| Watch a live net | — | — |
| Favorite a net, use **My Nets** | ✅ | — |
| Check yourself in to a net | ✅ | ✅ |
| Own and run a net | ✅ | ✅ |

If you only want to watch, stop here and go straight to [join a net](join-a-net.md).

## Prerequisites

- An email address you can read right now.
- Your callsign, if you intend to check in or run nets.

## Steps

### Step 1: Request a magic link

Open the instance and choose **Sign in**. Enter your email address and request a link.

NetRoll has no passwords. The same flow registers a new account and signs in an existing one,
so there's no separate sign-up form to find.

### Step 2: Open the link

Open the email and follow the link, in the same browser you requested it from.

The link is single-use and short-lived. Opening it verifies your email and signs you in. Nothing
about your account is usable until you do.

### Step 3: Accept the terms

On first sign-in you're asked to accept the terms. Consent is recorded with a timestamp.

This gate is deliberate: actions that write to a net stay blocked until you pass it. You won't
be asked again.

### Step 4: Reserve your callsign

Go to **Profile** and enter your callsign.

NetRoll validates international formats and normalizes portable suffixes (`/P`, `/M`). A
callsign another account already holds is rejected.

Callsigns are self-asserted — NetRoll doesn't check licence records. Reserving one means you got
there first on this instance, nothing more.

### Step 5: Fill in your profile

Set your display name, location, and Maidenhead grid while you're here. Your avatar defaults to
Gravatar from your account email; upload one to override it.

This matters more than it looks. **When staff log a check-in for you, your profile values
override whatever the callbook lookup returns** — so this is how you control the way you appear
in other operators' logs.

## Verify

Open your profile. Your email shows as verified and your callsign is reserved.

Then open any live net. The **Check in** action is available rather than routing you back to
callsign setup — that's the signal everything is in place.

## Next steps

Pick the path that matches why you're here:

- **[Run your first net](run-your-first-net.md)** — create a net, take it live, log check-ins.
- **[Join a net](join-a-net.md)** — find a running net and check yourself in.

Ongoing account management lives in [your account](../account/index.md): changing your email,
storing QRZ credentials, exporting or deleting your data.

If a link never arrived or was rejected, see
[troubleshoot signing in](../troubleshooting/signing-in.md).
