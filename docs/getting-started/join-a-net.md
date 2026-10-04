---
title: "Join a net"
description: "Find a live net on NetRoll, watch the roster update in real time, and check yourself in so net control knows you're there."
docType: "quickstart"
audience: ["participant"]
lastVerified: "2026-09-02"
---

# Join a net

Find a net that's running, watch it live, and check yourself in. Watching needs no account at
all; checking in needs a verified email and a callsign.

**Time:** ~3 minutes

## Prerequisites

- **Nothing, to watch.** Live sessions are publicly readable.
- **To check yourself in:** [an account with a reserved callsign](create-an-account.md). You can
  start watching first and set that up when you're ready.

## Steps

### Step 1: Find a net

Open the instance's landing page. The **active now** hero lists every net running at this
moment. Below it, the upcoming list shows scheduled nets. You can **filter** it by name, band,
mode, connection kind, geography, category, and net type, and **sort** it by time, name,
category, or net type. A net matches a band, mode, or connection-kind filter when any one of its
ways in does — so a net you can only reach over EchoLink, which has no band and no mode, is found
by choosing **EchoLink** under **Connection kind**. Each upcoming net shows the first couple of
lines of its description under the title when its owner wrote one — open the net's own page
for the whole thing.

The filters and sort you pick go into the address bar, so the list you are looking at is
already a link: bookmark it, or send it to someone and they open the same filtered view. If
a link you were sent carries anything the page does not use — a tracking parameter, or a
misspelled filter — the page still loads and says which parameters it ignored.

Times render in your local timezone as relative values — "in 2h", "40m ago" — not raw UTC.

If someone sent you a link to an unlisted net, open that link instead. Unlisted nets never
appear in discovery. See [net visibility](../nets/net-visibility.md).

### Step 2: Watch the session

Open a live net. You see every way the net is reachable, the roster of stations that have checked in,
and the working-station highlight showing who net control is working right now.

The connection status pill tells you whether you're **live**, **catching up** after a
reconnect, or **out of sync**. It always shows a colour, an icon, and a label together.

### Step 3: Check yourself in

Choose **Check in**. Your entry appears immediately as pending, then settles once the server
confirms it, and everyone watching sees it with a **Self** source badge.

If you haven't [created an account](create-an-account.md) or reserved a callsign yet, NetRoll
routes you through that and returns you to the check-in — you won't lose your place.

### Step 4: Set your staying status

Tell net control what to do with you:

| Status | What it tells net control |
|--------|---------------------------|
| **Staying for comments** | Come back to you in the rounds. You'll be worked again after the initial check-ins. |
| **In and out** | Log the contact and move on. You're not waiting around for comments or traffic. |

Change it at any time, or check yourself out entirely — both apply to your own entry only.

Everything else on your entry is staff-only. Signal report, notes, precedence, and role are set
by net control or a logger, and the server refuses your attempt regardless of what the UI shows
you. That's deliberate: the log is net control's record of the net, not a self-service form.

## Verify

Your callsign is in the roster with a **Self** source badge. When you are the next station net
control will call, the your-turn indicator lights on your row — a cyan left bar, a **You** chip,
and "You're next up". If net control has worked stations sinking to the bottom, that is the top
of the group still waiting, even when it sits above the station being worked right now.

## Next steps

- **[Reserve a callsign](../account/reserve-a-callsign.md)** — format rules and changing it later.
- **[Sign in](../account/sign-in.md)** — how magic links work and what to do when one fails.
- **[Session lifecycle](../live-sessions/session-lifecycle.md)** — what happens when a net
  pauses or closes.
- **[Troubleshooting live sessions](../troubleshooting/live-sessions.md)** — when the roster
  stops updating.
