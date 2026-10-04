---
title: "Start and close a session"
description: "Take a NetRoll net live, retune one of its ways in mid-net, and close the session to produce its summary and exports."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-09-14"
---

# Start and close a session

Take a net live so stations can check in, and close it when the net is over. Starting and
closing are owner actions, enforced server-side.

## Prerequisites

- [A net definition you own](../nets/create-a-net.md), with its ways in already set up.

## Steps

### Step 1: Start the session

Open the net and choose **Start Net**. There is nothing to fill in.

The session takes a snapshot of the definition as it stands right now — its **ways in**
included, copied by value — and renders from that snapshot for its whole life. Editing the net's
connection list afterwards does not move a session that is already running, which is what lets
you fix a typo in the net without repainting a net that is on the air.

Starting no longer asks for an operating frequency. A net can be reachable several ways at once,
or in no radio way at all, so there is no single frequency for a session to hold — each way in
carries its own.

The session is now `live`. It appears in the active-now hero and is publicly readable.

### Step 2: Retune a way in mid-net

If you QSY, use the frequency control on the session. Pick **which way in** moved — the picker
appears only when the net has more than one way that can be retuned — enter the new frequency in
MHz, and choose **Set frequency**. Ways with no frequency of their own, such as EchoLink or a DMR
talkgroup, are not offered and cannot be retuned.

The change propagates to every connected viewer within about two seconds, and it moves that one
way in and no other. It does not edit the net definition.

Only the net control station can retune. Anyone else gets HTTP 403 and no event is recorded.

### Step 3: Close the session

Choose **Close Net**. NetRoll appends a close event, moves the session to `closed`, and
stops accepting mutations.

The on-screen summary renders from the folded event log, so it reflects the session exactly as
it ran.

## Verify

After close, the session shows its summary, and the roster is frozen — attempting a check-in or
an edit is refused. The session no longer appears in the active-now hero.

## If you drop out

You don't have to close a session for it to end cleanly. If net control's heartbeat stops, the
session pauses rather than ending, another qualified operator can claim control, and an
abandoned session auto-closes after 15 minutes. See
[hand off net control](hand-off-net-control.md) and the
[session lifecycle](session-lifecycle.md).

## Related guides

- [Log check-ins](log-check-ins.md)
- [Export a log](export-a-log.md)
- [Configure delivery](../nets/configure-delivery.md) — what gets sent on close
- [Session lifecycle](session-lifecycle.md)
