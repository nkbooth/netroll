---
title: "Roles and permissions"
description: "NetRoll's per-net role hierarchy, what each role may do in a session, how ownership and platform admin differ from session roles, and how authorization is enforced."
docType: "reference"
audience: ["operator", "contributor"]
lastVerified: "2026-09-02"
---

# Roles and permissions

Who may do what on a NetRoll net. Three separate axes decide this — net ownership, session role,
and platform admin — and they don't imply each other.

## The three axes

| Axis | Scope | Granted by |
|------|-------|-----------|
| Ownership | One net definition | Another owner of that net |
| Session role | One session | An Owner or NCS of that session |
| Platform admin | The whole instance | The instance's `ADMIN_ACCOUNT_EMAILS` configuration |

Platform admin is not a super-role over nets. Its capability set is deliberately bounded and
separate.

## Role hierarchy

Capabilities nest: **Owner ⊃ NCS ⊃ Logger ⊃ Relay ⊃ Participant**.

| Role | What it is |
|------|-----------|
| Owner | Owns the net definition. Can start and close sessions and manage everything below. |
| NCS | The single active net control station for a session. |
| Logger | Logs and edits check-ins alongside net control. |
| Relay | Logs check-ins for stations net control can't hear. |
| Participant | A signed-in operator with a callsign, acting on their own entry only. |
| Viewer | Anyone reading a session. No account required. |

A session has exactly one active NCS at a time, decided server-side.

> **The Relay role and the "relayed by" field are different things.** The **role** is an account
> you grant, so that someone else can log check-ins on this session. The **field** is a record on
> one check-in naming **which station** passed that entry's traffic — usually a station with no
> NetRoll account at all. Neither implies the other: a Logger sitting beside net control can
> record that W1ABC relayed a station, and an account holding the Relay role can log a check-in
> nobody relayed. Recording the field needs no extra permission beyond the one that lets you log
> or edit the check-in in the first place. See [Log check-ins](../live-sessions/log-check-ins.md)
> and the glossary's three `Relay` rows.

## Capability matrix

| Action | Owner | NCS | Logger | Relay | Participant | Viewer |
|--------|:-----:|:---:|:------:|:-----:|:-----------:|:------:|
| View a live session | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Edit the net definition | ✅ | — | — | — | — | — |
| Configure delivery targets | ✅ | — | — | — | — | — |
| Manage co-owners | ✅ | — | — | — | — | — |
| Start a session | ✅ | — | — | — | — | — |
| Close a session | ✅ | — | — | — | — | — |
| Retune one of the session's ways in | ✅ | ✅ | — | — | — | — |
| Hand off net control | ✅ | ✅ | — | — | — | — |
| Claim control of a stalled session | ✅ | — | ✅ | — | — | — |
| Add a check-in for another station | ✅ | ✅ | ✅ | ✅ | — | — |
| Check yourself in | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| Edit your own entry's staying status | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| Edit another station's entry | ✅ | ✅ | ✅ | — | — | — |
| Set a signal report | ✅ | ✅ | ✅ | — | — | — |
| Set precedence or traffic | ✅ | ✅ | ✅ | — | — | — |
| Set the working station | ✅ | ✅ | — | — | — | — |
| Reorder the roster | ✅ | ✅ | — | — | — | — |
| Set the net-level note | ✅ | ✅ | ✅ | — | — | — |
| Remove or block a station | ✅ | ✅ | — | — | — | — |
| Grant or revoke session roles | ✅ | ✅ | — | — | — | — |
| Export CSV or ADIF | ✅ | ✅ | — | — | — | — |

Checking yourself in additionally requires a verified email and a reserved callsign.

## Staff-only fields

A participant may edit their own entry's staying status and check themselves out. Everything
else on an entry is staff-only, and the server rejects a participant's attempt regardless of any
client-side affordance:

- Signal report
- Per-station notes, both the staff note and the public note
- Precedence and traffic
- Role
- Any field on another user's entry

## Constraints

**Grants are session-scoped.** A role granted on one net carries no authority on any other. A
granted operator acting on a different net is refused with HTTP 403, enforced at the object
level.

**A role may only manage roles below its own level.** An attempt to manage a role at or above
the acting role's level is denied.

**Nothing client-supplied is trusted.** Every net-scoped mutation is authorized server-side from
the tuple of net, target object, and the acting role's capabilities. A hidden UI control is a
convenience, never a security boundary.

**Ownership doesn't expire, session grants don't persist.** Ownership lives on the definition.
Session role grants apply to the session they were made on — grant them again next time.

## Platform admin capabilities

Bounded and audit-logged. There is no unbounded superuser path.

| Capability |
|-----------|
| List abuse reports |
| Resolve an abuse report |
| Disable an account |
| Re-enable an account |
| Read the security audit log |

An address in `ADMIN_ACCOUNT_EMAILS` grants nothing until an account registers under it. With
no addresses configured, the admin surface is unreachable and the app boots normally.

## See also

- [Grant roles and relays](../live-sessions/grant-roles-and-relays.md)
- [Manage owners](../nets/manage-owners.md)
- [Moderate a session](../live-sessions/moderate-a-session.md)
- [HTTP API](http-api.md)
- [Configuration](configuration.md#platform-admins)
