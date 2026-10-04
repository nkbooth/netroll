---
title: "Hand off net control"
description: "Transfer net control on a live NetRoll session, claim control when the NCS drops out, and understand the paused, resumed, and auto-closed outcomes."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-08-30"
---

# Hand off net control

Move net control to another operator — either deliberately, or because the current NCS dropped
out. A session has exactly one active NCS at a time, decided by the server.

## Prerequisites

- A [live session](start-and-close-a-session.md).
- To hand off deliberately: the NCS role.
- To claim a stalled session: the Owner or Logger role on it.

## Steps

### Step 1: Hand off deliberately

As NCS, transfer control to another owner or qualified operator.

Control moves without interrupting the live stream. Viewers stay connected, the roster stays
put, and the change is reflected for everyone.

### Step 2: Recognize a stalled session

If the NCS's heartbeat stops for longer than the configured threshold, NetRoll pauses the
session on its own. Every viewer sees the slate **net-paused** status. The roster freezes and
writes are suspended — nothing is lost, nothing moves.

### Step 3: Claim control

As a co-owner or logger on a stalled session, claim the control handoff. Control transfers to
you and the session resumes.

### Step 4: Let the original NCS resume

If the original NCS reconnects before the session auto-closes, they can resume control.

### Step 5: Or let it auto-close

A session with no active NCS for 15 minutes is closed by a background job, which produces the
summary exactly as a manual close would. The job is idempotent, so it never closes a session
twice or produces a duplicate summary.

## Verify

After a handoff, the new NCS can retune a way in and set the working station,
and the previous NCS cannot. Viewers never saw a disconnect.

## What happens when

| Situation | Result |
|-----------|--------|
| NCS hands off deliberately | Control moves immediately; the stream is uninterrupted. |
| NCS heartbeat missed past the threshold | Session pauses. Roster frozen, writes suspended, net-paused shown to all. |
| Owner or logger claims control | Session resumes under the new NCS. |
| Original NCS returns before auto-close | They may resume control. |
| No active NCS for 15 minutes | A background job closes the session and produces its summary. |

## Related guides

- [Session lifecycle](session-lifecycle.md)
- [Grant roles and relays](grant-roles-and-relays.md)
- [Start and close a session](start-and-close-a-session.md)
- [Troubleshooting live sessions](../troubleshooting/live-sessions.md)
