---
title: "Create a net"
description: "Create a NetRoll net definition with the ways it can be reached, its geography, category, and net type, then edit it without disturbing sessions already running."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-09-02"
---

# Create a net

Create the reusable definition that sessions of your net are started from. Do this once per
net, not once per occurrence.

## Prerequisites

- A signed-in account with [a reserved callsign](../account/reserve-a-callsign.md) — net
  ownership is gated on holding one.
- Room under your account's net cap. Instances cap the number of active nets one account may
  own; the default is 7. See [`MAX_NETS_PER_USER`](../reference/configuration.md#resource-caps).

## Steps

### Step 1: Open the net form

Go to **New net**.

### Step 2: Describe the net

Fill in the title and description. Both are shown in discovery, so make the title the name
people already call the net on the air, and use the description to say what the net is for —
listings show its first couple of lines, which is often what tells two similarly-named nets
apart.

### Step 3: Set the operating details

One of these changes how the app behaves during a session. The rest are descriptive — they
affect how people find the net, not how it runs.

**Changes how the session works:**

| Field | What it changes |
|-------|-----------------|
| **Net type** | *Open* — stations call in freely as they hear you. *Roll call* — you work the roster in order. This is the working style of the net; it shapes how you and your loggers use the console. |

**Descriptive only — affects discovery and display:**

| Field | What it changes |
|-------|-----------------|
| Net category | How the net is classified in discovery listings. |
| Description | Shown as a short clamped preview in discovery and My Nets listings, and in full on the net's own page. Paragraph breaks you write are kept on that page; the listing preview collapses them to fit two lines. If you leave several empty lines in a row, they are shortened to a single blank line so the page stays readable. |
| Expected duration | Displayed alongside the schedule so operators know what they're committing to. |

Frequency, band and mode are not fields of the net itself. They belong to each way the net can
be reached, which you add in the next step — a net on 40m SSB with an EchoLink node has one
frequency, one band and one mode, all on its HF entry, and none on its EchoLink entry.

### Step 4: Add the ways to reach the net

Every net has at least one way to reach it, and you add the first one here. The **Ways to reach
this net** panel is a list, and each entry shows only the fields its own kind has:

| Kind | What it asks for |
|------|------------------|
| Frequency | Frequency, band, mode |
| Repeater | Frequency, band, mode, offset, tone mode, tone value |
| EchoLink, AllStar | The node |
| DMR | The network and the talkgroup. The network box suggests Brandmeister, TGIF and FreeDMR but accepts any name — type your own if yours is not listed. Leave it blank if you would rather not say; only the talkgroup is required |
| D-Star, System Fusion, URF | The reflector |

The **Mode** on an RF entry changes how the session works: it sets the signal-report input
format your loggers get — RS for phone, RST for CW, dB-SNR for digital, qualitative or none for
FM and repeater. Set it wrong and staff enter reports in the wrong format for the whole net.
**Band** is a structured value discovery filters on.

The panel works the same way here as it does on the edit page — see
[Edit the ways to reach a net](#edit-the-ways-to-reach-a-net) below for adding, removing and
reordering entries, and for what the order means.

### Step 5: Set the geography

Enter country, state, and Maidenhead grid. NetRoll validates the grid string and rejects a
malformed one with a field-level message. Discovery filters on these values.

### Step 6: Choose visibility

Leave visibility at **Listed** to have the net appear in discovery. Choose **Unlisted** to make
it reachable only through a non-guessable link token — see
[net visibility](net-visibility.md).

### Step 7: Save

Save the definition. To share an unlisted net afterwards, open it and copy its link-token URL;
anyone with that URL can reach the net, and a request without a valid token is refused.

## Edit the ways to reach a net

Saving takes you to the net's edit page, where the same **Ways to reach this net** panel you
filled in at creation carries on. Add, remove and reorder entries as the net actually works. Two things follow from the order:
it is the order the net's page and your listings show, and **the first entry is the one an
ADIF export describes the QSO with**. There is no separate control for that — move an entry to
the top to change it.

A net must keep at least one way to reach it, so the last remaining entry cannot be removed.
Add its replacement first.

If your net uses something none of those kinds names, use **My net uses something the list
doesn't name** and describe it. Reach for it only when nothing else fits: how often it gets
used is how NetRoll finds out the list is cut wrong.

Some connections are marked as ones NetRoll could not classify — usually a reflector imported
from the older, single-connection form, where nothing recorded which network it belonged to.
Pick the network from that entry's list and NetRoll files the value properly. Picking the wrong
one is undoable — the same list offers a way back to leaving it unclassified.

A net can hold at most 32 ways to reach it. At the ceiling the add buttons switch off until you
remove one.

If someone else edits the same net while you have it open, your save is refused rather than
silently overwriting theirs. NetRoll re-reads the net, keeps what you typed, and folds in what
they changed — including any way to reach the net they added, which is then part of your list.
Save again and it all lands.

## Verify

Open the discovery landing page. A Listed net appears in the upcoming list once you've
[scheduled it](schedule-a-net.md). An Unlisted net does not appear there at all — open its link
token URL instead and confirm it loads.

## Editing a net later

Editing a definition increments its version and preserves the prior one for provenance.

Editing never disturbs a session that has already started. A session snapshots the definition at
the moment it goes live and renders from that snapshot, so a mid-net edit changes only *future*
occurrences. Sessions already run stay attributable to the definition version they were started
from.

Only owners can edit or delete a definition. The server checks ownership on every request and
returns HTTP 403 otherwise, regardless of what the client sends.

## Related guides

- [Schedule a net](schedule-a-net.md)
- [Manage owners](manage-owners.md)
- [Configure delivery](configure-delivery.md)
- [Start and close a session](../live-sessions/start-and-close-a-session.md)
