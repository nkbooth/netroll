---
title: "Troubleshoot live sessions"
description: "Fix NetRoll live session problems: rosters that stop updating, paused nets, rows locked by another operator, rejected edits, and check-ins that won't commit."
docType: "troubleshooting"
audience: ["operator", "participant"]
lastVerified: "2026-09-14"
---

# Troubleshoot live sessions

Problems that show up while a net is running, grouped by what's on your screen. Most of them are
the system telling you something deliberately rather than failing.

## "Reconnecting — catching up…"

**Symptoms:** The banner says catching up and the connection pill reads *catching up*.

**Cause:** Your connection dropped and reconnected. The client is replaying the events it missed
from its last known `seq`.

**Fix:**

1. Wait. The replay is automatic and usually takes a moment.
2. Don't refresh — refreshing throws away the resume cursor and forces a full snapshot instead.

**Verify:** The pill returns to *live* and the roster matches what other operators see.

---

## "Lost the server — showing last known roster"

**Symptoms:** The banner offers a resync and the pill reads *out of sync*.

**Cause:** The client can't reach the server at all. What you're looking at is the last state it
folded, held deliberately rather than blanked.

**Fix:**

1. Check your own network first.
2. Use the resync the banner offers.
3. If resync keeps failing, the instance is down. Nothing is lost — the event log is
   authoritative in Postgres and the roster rebuilds from it when the server returns.

**Verify:** The pill returns to *live*.

---

## The net is paused and the roster is frozen

**Symptoms:** Everyone sees a slate *net paused* status. Writes are refused.

**Cause:** This is not a connection problem. Net control's heartbeat stopped, so NetRoll paused
the session to keep the log consistent.

**Fix:**

1. If you're the NCS and you have reconnected, resume control.
2. If you're a co-owner or logger and the NCS isn't coming back, claim control. The session
   resumes under you.
3. If nobody claims it, a background job closes the session after 15 minutes and produces its
   summary.

**Verify:** The paused status clears and check-ins commit again.

See [hand off net control](../live-sessions/hand-off-net-control.md).

---

## A row is read-only and says another operator is editing

**Symptoms:** You can't edit an entry; it shows someone else's callsign as the editor.

**Cause:** They hold the soft lock on that entry. Locks run on a roughly 15-second sliding lease
renewed by activity.

**Fix:**

1. Wait for them to finish. The lock releases on its own.
2. If they disconnected, the lock releases immediately when their heartbeat drops — no waiting
   out the lease.

**Verify:** The row becomes editable.

---

## Your edit was rejected with a conflict

**Symptoms:** Saving an entry returns HTTP 409 or a conflict message.

**Cause:** Two operators committed against the same version of that entry. NetRoll rejects the
stale write rather than silently discarding someone's edit.

**Fix:**

1. Re-open the entry to see the current server state.
2. Re-apply your change on top of what's there.
3. Save again.

**Verify:** The save succeeds and the entry shows both changes.

---

## A check-in won't commit

**Symptoms:** Your entry stays pending, or the commit is refused.

**Cause:** Several distinct causes look the same on screen.

**Fix:**

1. Check the connection pill. A pending entry that never settles means the write hasn't reached
   the server.
2. Check whether the net is paused. Writes are suspended while it is.
3. Check whether the session is closed. A closed session accepts no further mutations.
4. If you're checking yourself in, confirm you have a verified email and a reserved callsign.
5. If you're logging for someone else, confirm your role allows it — see
   [roles and permissions](../reference/roles-and-permissions.md).

**Verify:** The entry appears in the roster with the correct source badge.

---

## Viewers see a different roster from yours

**Symptoms:** Your console shows an entry that other people don't.

**Cause:** Almost always the pending state working as designed. The client that made a change
shows its own entry as pending immediately; viewers render authoritative events only, so nobody
ever sees another client's unconfirmed write.

**Fix:**

1. Wait for the entry to settle from pending to confirmed.
2. If it never settles, treat it as a check-in that won't commit, above.

**Verify:** The entry is present for everyone with the same `seq` ordering.

---

## Export is refused

**Symptoms:** Downloading CSV or ADIF returns HTTP 403.

**Cause:** Export is an NCS and owner capability. It's also only available on a closed session.

**Fix:**

1. Confirm the session is closed.
2. Confirm you hold the NCS or Owner role on it.
3. If you need the log but not the role, ask an owner to
   [configure delivery](../nets/configure-delivery.md) so summaries reach you automatically.

**Verify:** The download starts and its row count matches the on-screen summary.

---

## Get help

File an issue at
[github.com/nkbooth/netroll/issues](https://github.com/nkbooth/netroll/issues).
Include the session's state at the time, what the connection pill read, and whether other
viewers saw the same thing.

## Related pages

- [Live updates](../live-sessions/live-updates.md)
- [Session lifecycle](../live-sessions/session-lifecycle.md)
- [Hand off net control](../live-sessions/hand-off-net-control.md)
- [Roles and permissions](../reference/roles-and-permissions.md)
