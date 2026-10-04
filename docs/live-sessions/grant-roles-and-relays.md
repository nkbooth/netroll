---
title: "Grant roles and relays"
description: "Grant logger and relay roles on a NetRoll session so other operators can log check-ins, including stations net control can't hear."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-09-02"
---

# Grant roles and relays

Bring other operators into the console. A logger shares the typing; a relay checks in stations
you can't hear from your location.

> This page is about the **role** — an account you let log check-ins on this session. To record
> **which station relayed one particular check-in**, use the **Relayed by** field on the entry;
> see [Log check-ins](log-check-ins.md#step-6a-record-who-relayed-the-station). Neither implies
> the other.

## Prerequisites

- A [live session](start-and-close-a-session.md).
- The Owner or NCS role on that session — a role may only manage roles below its own level.

## Steps

### Step 1: Open the session's roles

Open the session's role management.

### Step 2: Grant the role

Grant **Logger** to an operator who should log check-ins alongside you, or **Relay** to a
station that will pass along check-ins from operators outside your range.

The grant is scoped to this session. A role carries no authority on any other net, and the
server enforces that at the object level — a granted operator acting on a different net is
refused with HTTP 403.

### Step 3: Let the relay work

A relay logs check-ins into the same session you're running. Each entry is attributed to the
relaying operator and carries a **Staff-entered** source badge, so the log shows who put it
there.

### Step 4: Revoke when the net ends

Revoke a grant when you no longer want that operator logging. Grants don't carry over to the
next session of the net — grant them again next time.

## Verify

Ask the granted operator to log a check-in. It appears in the roster attributed to them. Ask
them to attempt something above their level — closing the session, or granting a role — and the
server refuses it.

## Role hierarchy

Capabilities nest: **Owner ⊃ NCS ⊃ Logger ⊃ Relay ⊃ Participant**. Every authorization decision
is made server-side from the acting role's capabilities against the specific object; no
client-supplied role or id is trusted.

See [roles and permissions](../reference/roles-and-permissions.md) for the full capability
table.

## Related guides

- [Log check-ins](log-check-ins.md)
- [Hand off net control](hand-off-net-control.md)
- [Manage owners](../nets/manage-owners.md) — definition ownership, a separate axis from
  session roles
- [Moderate a session](moderate-a-session.md)
