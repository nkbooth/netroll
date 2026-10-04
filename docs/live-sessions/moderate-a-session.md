---
title: "Moderate a session"
description: "Remove a check-in or block a disruptive account from a live NetRoll session, with blocks enforced against the account rather than a callsign string."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-07-31"
---

# Moderate a session

Remove an entry or block a disruptive operator from your net. Moderation is an NCS capability;
Loggers, Relays, and Participants are refused by the server.

## Prerequisites

- A [live session](start-and-close-a-session.md).
- The NCS or Owner role on that session.

## Steps

### Step 1: Remove a check-in

Remove the entry from the roster. The removal is recorded as an event and propagates live to
every viewer.

Removal takes the entry out of this session. It doesn't stop the operator from checking back in.

### Step 2: Block an account

To stop someone rejoining, block the registered account behind the entry.

The block is enforced against the **account**, not the callsign string, so the same user cannot
come back under a different callsign for this session.

## Verify

The removed entry is gone from the roster in every connected client within about two seconds. A
blocked account attempting to check back in to this session is refused.

## Limits

**Blocks are session-scoped.** A block applies to the session you're running, not to the whole
net or the whole instance.

**Account-less callsigns can be removed but not blocked.** A block needs an account to attach
to. An entry logged for a callsign with no NetRoll account can be removed, but there's nothing
to block.

**Instance-level abuse is a separate path.** For behaviour that goes beyond one net, use the
**Report abuse** affordance. Reports are recorded and actioned by instance admins from the
[admin dashboard](../self-hosting/admin-dashboard.md) through a deliberately bounded set of
actions — view and resolve reports, look up an account, disable and re-enable accounts, and read
the audit log — and every admin action is audit-logged. There is no unbounded superuser path.

## Related guides

- [Grant roles and relays](grant-roles-and-relays.md)
- [Roles and permissions](../reference/roles-and-permissions.md)
- [Hand off net control](hand-off-net-control.md)
