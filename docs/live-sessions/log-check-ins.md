---
title: "Log check-ins"
description: "Log stations into a live NetRoll session using the pinned quick-add row, record mode-shaped signal reports, staying status and which way in each station arrived on, and correct entries in the detail modal."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-09-02"
---

# Log check-ins

Log stations as they call in, and capture the full record for each one. The quick-add row is
built for the hot path — keeping up with a pileup while you're also working a radio.

## Prerequisites

- A [live session](start-and-close-a-session.md).
- The NCS or logger role on that session. Relays can log check-ins too; see
  [grant roles and relays](grant-roles-and-relays.md).

## Steps

### Step 1: Use the quick-add row

The quick-add row is pinned at the top of the roster — never behind a button — and the callsign
field holds focus when the console opens.

Tab order is **Callsign › Name › Location › Grid › Report › Staying › Save**.

Press `n` from anywhere on the page to jump focus back to the callsign field.

If the callsign you enter is already on tonight's roster, NetRoll tells you — both when you leave
the field and when you commit with Enter, so the fast path below gets the warning too. It names
the station and reads differently depending on the situation: whether the station is still
waiting to be worked, or whether you already worked them earlier in the net. After a commit the
warning stays up, naming the station you just logged, until you start typing the next callsign.
Either way it never stops you — the check-in goes in as its own entry, with its own report,
traffic and notes, which is exactly what a deliberate re-check should be.

### Step 2: Commit with Enter

Press **Enter** from any field to commit the entry. The fields clear and focus returns to
callsign, ready for the next station. Use **Save & close** when you're logging one station and
want to stop.

Your own entry shows as pending immediately and settles once the server confirms it. Other
viewers only ever see confirmed entries, never your pending ones.

### Step 3: Let the lookup fill the details

When you enter a callsign, NetRoll runs a best-effort callbook lookup and autofills name and
location. Everything it fills stays editable.

The lookup never blocks logging. If it fails, returns nothing, or times out, you type the
details yourself and carry on. If the station has a NetRoll profile, their profile values win
over the looked-up values. See [connect QRZ](../account/connect-qrz.md) to use your own QRZ
credentials for lookups.

A returning station is faster still: the net remembers callsigns that have checked in before
and prefills their remembered name and location, which you can accept or edit.

### Step 4: Record the signal report

The report input is shaped by the session's mode, and it's always optional:

| Mode | Report format |
|------|---------------|
| Phone | RS |
| CW | RST |
| Digital | dB SNR |
| FM / repeater | Qualitative, or none |

Signal reports are staff-only. A participant attempting to set one is refused by the server.

### Step 5: Set the staying status

Mark each station as **staying for comments** or **in and out**.

| Status | What it changes |
|--------|-----------------|
| Staying for comments | The station is worked again in the rounds. This is what tells you who's still waiting when you come back around. |
| In and out | The station is logged and done. It won't come up again in rounds. |

The roster shows the status with a colour, an icon, and a label together — never colour alone.
Both values reach the [CSV export](export-a-log.md); it's part of the record of how the net ran,
not just a live-view convenience.

A participant can set this on their own entry. Everything else on a check-in is staff-only.

### Step 6: Record which way in the station arrived on

A net can be reachable several ways at once — an HF frequency, a repeater, an EchoLink node, a
DMR talkgroup. NetRoll records which one each station actually came in on, and it does it
without a dropdown per station.

**Set the stamp once per block.** The toolbar carries a **Taking check-ins on** control. Pick
the connection you're working, and every check-in you commit after that records it. The control
always states what's in force, so you never have to remember what you set.

**Override for one station with one tap.** The quick-add row shows the same choice, preselected
to the stamp. When a station arrives on a different connection, tap that connection before you
commit. It applies to that check-in only — the next station is back on the block's connection
with no second action.

**Type a way in the net doesn't list.** Choose **Other…** and type it — "Bob's hotspot", "phone
patch". Up to 64 characters, one line. It's recorded against that check-in and nothing else: it
does not add a connection to the net, and the owner's connection list stays theirs.

**Nothing is recorded until you say so.** Before you set a stamp, check-ins carry no way in at
all, and every surface says so rather than guessing. NetRoll never fills this in for you, not
even on a net with a single connection — "came in on HF" and "not recorded, on a net that only
has HF" are different facts, and only the second stays true if the owner later adds another way
in. In a [CSV export](export-a-log.md) the way-in cell stays empty for those check-ins; an ADIF
export is the one place NetRoll fills the gap, describing the QSO with the net's own connection
because the format has nowhere to say "unrecorded".

The stamp is yours alone and lives only in this browser tab: a second operator logging the same
net sets their own, and reloading the page clears yours back to nothing recorded. That is
deliberate — a restored stamp whose age you can't see is how a whole block of check-ins ends up
claiming the wrong band.

You can also set or correct the way in later, from the detail modal below.

### Step 6a: Record who relayed the station

When a station checks in through a relay — another operator hears them, you don't, and that
operator passes their traffic along — put the **relaying station's callsign** in **Relayed by**,
below the Save buttons in the quick-add row.

**It is a different question from the way in, and NetRoll keeps them apart on purpose.** The way
in records *how* the traffic travelled; Relayed by records *who passed it*. A station can have
both ("came in on the repeater, relayed by W1ABC"), either, or neither, and correcting one never
disturbs the other.

**The relaying station does not need a NetRoll account.** It is a callsign you type, not a person
you pick from a list — usually someone who is simply on the air. NetRoll normalises what you type
the same way it normalises a check-in callsign.

**It is also not the Relay role.** Granting someone the [Relay role](grant-roles-and-relays.md)
lets *them log check-ins on this session*. This field records *which station passed one entry's
traffic*. You can record it whatever your own role, as long as you can log or edit the check-in;
and an operator holding the Relay role logs plenty of check-ins that nobody relayed.

**Nothing sticks between stations.** Unlike the way-in stamp, this box clears after every save.
Each relayed station was relayed by whoever happened to hear it, so carrying the last callsign
forward would put a confidently wrong one on the next entry — worse than leaving it blank.

The relaying station is **not** shown on the public page. It names a third party who never
checked in, so it stays on the operator console, the CSV export and the on-close webhook.

You can set or correct it later from the detail modal, in the **Relayed by** field.

### Step 7: Open the detail modal for the full record

Open an entry to view and edit everything NetRoll captures: callsign, name, location and grid,
role, source, entering operator, signal report, staying status, precedence and traffic, the way
in, the relaying station, timestamps, and the two per-station notes. Press **Esc** to cancel.

The notes are two separate fields. **Staff note (operators only)** is private to your logging
team. **Public note (everyone watching)** appears on the roster row for every viewer of the live
page, including someone with no account. Notes written before the split are all staff notes —
nothing you had already written became public. See
[Capture notes](work-the-roster.md#step-5-capture-notes).

Changing a field annotates the prior value, so a correction reads as
"Correcting grid: FN25 (was FN35)" rather than silently overwriting history.

### Step 8: Set precedence when it matters

On a traffic or emergency net, set each check-in's precedence, with an optional traffic
indicator.

| Precedence | What it changes |
|------------|-----------------|
| **Emergency** | Red chip. Sorts to the top when you order the roster by precedence. |
| **Priority** | Amber chip. Sorts above Routine, below Emergency. |
| **Routine** | Neutral chip. The default; sorts last. |

Setting precedence does **not** reorder the roster on its own — it labels the entry. You choose
when to apply that order, from [work the roster](work-the-roster.md). Within a tier, check-in
order is preserved, so nobody loses their place.

Precedence and the traffic indicator are staff-only, and both land in the
[export](export-a-log.md). On a routine net you can ignore this entirely.

## Verify

Each entry you logged shows a **Staff-entered** source badge and records which operator entered
it. Stations that checked themselves in show a **Self** badge instead.

Open the session in a second window. Every entry appears there in the same order, within about
two seconds of you committing it.

## Logging stations without accounts

Log any callsign, whether or not it belongs to a NetRoll account. The entry is created and sits
in the roster alongside registered users.

## Troubleshooting

**A row is read-only and says someone is editing.** Another operator holds the soft lock on that
entry. Locks run on a roughly 15-second sliding lease renewed by activity, and release
immediately if the holder's connection drops.

**Your edit was rejected with a conflict.** Two operators committed against the same version.
NetRoll rejects the stale write rather than losing an edit; re-open the entry to see the current
server state and re-apply your change.

## Related guides

- [Work the roster](work-the-roster.md)
- [Grant roles and relays](grant-roles-and-relays.md)
- [Moderate a session](moderate-a-session.md)
- [Roles and permissions](../reference/roles-and-permissions.md)
