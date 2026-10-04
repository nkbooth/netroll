---
title: "Run your first net"
description: "Create a net definition, start a live session, and log your first check-in on NetRoll. Covers sign-in, callsign reservation, and the operator console."
docType: "quickstart"
audience: ["operator"]
lastVerified: "2026-09-14"
---

# Run your first net

Create a net, take it live, and log a station checking in. By the end you'll have a live
session other operators can watch in real time.

**Time:** ~5 minutes

## Prerequisites

- **[An account with a reserved callsign](create-an-account.md)** — net ownership is gated on
  holding one. Takes about three minutes if you don't have one yet.
- A NetRoll instance — the public one, or [your own](../self-hosting/quickstart.md). The public
  instance is operated on a best-effort basis with no availability guarantee — no SLA is offered —
  so a net that needs one should self-host.

## Steps

### Step 1: Create a net

Go to **New net**. Four choices here change how the net behaves later, so they're worth a
moment:

| Choice | What it changes |
|--------|-----------------|
| **Mode** | The signal-report input your loggers get during the session. Phone → RS, CW → RST, digital → dB-SNR, FM/repeater → qualitative. Pick wrong and you're typing reports in the wrong format all net. |
| **Net type** | *Open* — stations call in freely. *Roll call* — you work the roster in order. This is the working style of the net, not just a label. |
| **Visibility** | *Listed* — appears in discovery for anyone. *Unlisted* — reachable only by a link token, never listed. See [net visibility](../nets/net-visibility.md). |
| **Frequency (MHz)** | The frequency this net's first way in is planned for. A session copies it and can be retuned mid-net without editing the definition. |

Fill in the title — what operators see first in discovery — and leave visibility at **Listed**
for now. A description is optional and also shows in discovery, as a short preview under the
title. Everything else can be edited later without disturbing a net already running.

Save the net. The [full field set](../nets/create-a-net.md), including geography and repeater
details, is there when you need it.

### Step 2: Start the session

Open the net and choose **Start Net**. There is nothing to fill in — the session copies the
net's ways in as they stand right now, and you retune one of them from the session if you QSY.

The session is now `live`. It appears in the active-now hero on the discovery landing page,
and anyone — signed in or not — can open it and watch.

### Step 3: Log a check-in

The operator console opens with the quick-add row pinned at the top of the roster, focus
already in the callsign field.

Type a callsign and press **Enter**. The entry commits, the fields clear, and focus returns to
the callsign field ready for the next station. Press `n` from anywhere on the page to jump back
to that field.

## Verify

The station you logged appears in the roster with a **Staff-entered** source badge, and every
way the net is reachable shows in the header at the top of the session.

Open the same session URL in a second browser window — signed out is fine. The check-in you
logged is already there, and the next one you log appears within a couple of seconds without a
refresh. That's the live stream working.

## Next steps

- **[Log check-ins](../live-sessions/log-check-ins.md)** — signal reports, staying status,
  precedence, and the detail modal.
- **[Work the roster](../live-sessions/work-the-roster.md)** — the working-station cursor and
  running rounds.
- **[Schedule a net](../nets/schedule-a-net.md)** — put your net on a recurring schedule so it
  shows up in the upcoming list.
- **[Grant roles and relays](../live-sessions/grant-roles-and-relays.md)** — bring in a logger
  or a relay station.
- **[Export a log](../live-sessions/export-a-log.md)** — get CSV and ADIF out after you close.
