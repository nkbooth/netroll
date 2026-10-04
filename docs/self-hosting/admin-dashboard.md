---
title: "Use the admin dashboard"
description: "Review abuse reports, disable or re-enable an account, and read the security audit log from NetRoll's built-in admin dashboard at /admin."
docType: "howto"
audience: ["self-hoster"]
lastVerified: "2026-08-01"
---

# Use the admin dashboard

Review what people have reported and act on it, without leaving the browser. The dashboard is the
UI over NetRoll's bounded admin API — it can do exactly what that API can do and nothing more.

## Prerequisites

- [A running instance](quickstart.md).
- At least one address in `ADMIN_ACCOUNT_EMAILS`.
- An account signed in with one of those addresses.

## Grant yourself access

Admins are set at boot, not in the database. Set `ADMIN_ACCOUNT_EMAILS` to a comma-separated list
of addresses and restart:

```bash
ADMIN_ACCOUNT_EMAILS=you@example.com,cohost@example.com
```

An instance with nothing configured has **no** admin at all — the dashboard is unreachable and
every admin endpoint refuses everyone. The address is matched against the account's stored email
after trimming and lower-casing, so capitalisation does not matter. An entry that is not a valid
address stops the server at boot rather than silently granting nobody.

Sign in with a listed address and **Admin** appears in the header beside *Profile*. It is absent
for everyone else, and visiting `/admin` directly sends a non-admin back to the discovery page.

## Review and resolve reports

The **Reports** tab lists open abuse reports, oldest first — the report text, whatever contact the
reporter chose to leave, and the page they were on. Reports are free text: they name no account,
so read them and decide what, if anything, to act on.

**Resolve** clears a report from the queue. It does not notify the reporter and does nothing to any
account; it only records that the report has been dealt with.

## Find and disable an account

Reports do not link to accounts, so the **Accounts** tab is how you turn "this operator" into
something you can act on. Search by callsign, email address, or account ID.

The match is **exact**. A partial callsign finds nothing, by design — the lookup is for targeting a
specific account you already have an identifier for, not for browsing the user list.

**Disable** takes a second click to confirm, then:

- revokes every live session, so the account is signed out immediately;
- refuses any further authenticated action;
- refuses sign-in, so the holder cannot clear it by signing back in.

Disabling is **not** deletion. No data is removed, and the account keeps its callsign reservation.
**Re-enable** is the only thing that lifts it.

You cannot disable your own account. Because admin status comes from an environment variable rather
than the database, disabling yourself on a single-admin instance would lock you out permanently with
no way back in — so the server refuses it.

## Read the audit log

The **Audit log** tab shows every security-relevant action newest-first: who acted, what they did,
what they acted on, and when. It covers admin actions, sign-ins and sign-outs, role grants and
revocations, self-deletion requests, and net and session lifecycle — creation, edits, archival,
and session start and close.

A net edit records **which fields changed**, by name. It never records the values, and re-saving a
net without changing anything records nothing at all.

**Sessions closed automatically are not recorded.** When a net is abandoned and the server closes it
after 15 minutes, nobody performed that action, and the log only holds actions with an actor. A
session can therefore show a start with no close — check the session's own state rather than reading
the gap as "still running".

Opening the tab records a read of its own — reading the audit log is itself an audited action, so
expect a `view-audit-log` entry each time.

The log holds no email addresses, tokens, or credentials by design, so it is safe to share a screen
showing it.

## Verify

1. Sign in as a listed address and confirm **Admin** appears in the header.
2. File a report through the public [Report abuse](../troubleshooting/index.md) link, then confirm
   it appears in the Reports tab.
3. Open the Audit log tab and confirm your own actions are listed, with no address anywhere in it.
4. Create a net, then search for it by part of its title and pivot on **Actions on this** — you
   should see it created. Start and close a session on it; both appear under the same filter.

## What the dashboard deliberately cannot do

There is no unbounded administrator. The surface is exactly six actions — view reports, resolve a
report, search objects, disable an account, re-enable an account, and read the audit log — and
every one of them is recorded.

Search cannot enumerate the user list: without an exact email or ID, only a callsign or display-name
prefix will match.

In particular, an administrator **cannot** read anyone's stored QRZ credentials. They are encrypted
so that no endpoint can return them, and none tries.

## Related guides

- [Configuration](../reference/configuration.md)
- [HTTP API](../reference/http-api.md)
- [Quickstart](quickstart.md)
