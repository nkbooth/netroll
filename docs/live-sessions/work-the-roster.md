---
title: "Work the roster"
description: "Mark the station you're working in a NetRoll session, sink worked stations out of the way, run repeatable rounds, capture per-station and net-level notes, and reorder the roster."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-08-29"
---

# Work the roster

Show everyone which station you're working right now, and run rounds through the roster
capturing comments and traffic. The working cursor is what turns a list of check-ins into a net
people can follow.

## Prerequisites

- A [live session](start-and-close-a-session.md) with check-ins on it.
- The NCS role on that session.

## Steps

### Step 1: Mark the working station

Focus or select a roster row and press `w`, or use the inline set-working control.

At most one station is the working station at a time. Every viewer — including account-less
ones — sees the working cursor move to it live: a coral left bar, a wash across the row, and
the label "Working now".

The keycap legend and tooltips make `w` and `n` discoverable, so you don't have to remember
them.

### Step 2: Complete the station

When you've finished with a station for this round, mark it worked. The row drops to reduced
opacity with a green tick.

Where the row then sits depends on the ordering mode (Step 3). By default it **keeps its
position** — the roster order stays stable, so nobody loses their place. With **Sink worked
stations** turned on, it moves down to the worked group instead, and everyone's roster moves
with it.

### Step 3: Sink worked stations out of the way

On a long net, most of the roster is people you've already worked. Turn on **Sink worked
stations** and every station you finish drops below the ones still waiting — automatically, as
you work them. You only turn it on once; it stays on for the rest of the session, and it stays
on for whoever takes over the net control position from you.

The station you're working now always stays in the upper group with the stations still waiting,
never in the worked group — including on a second round, when you come back to someone you have
already worked. The upper group keeps its order, so the next station to call is the highest one
in it you have not worked yet. If you also order by precedence (Step 5), precedence applies
**within** each group — an emergency you've already worked does not jump back above the stations
still waiting.

The worked group collapses to a count — "Worked 7". Click it (or tab to it and press Enter) to
expand the stations inside; collapsing and expanding is yours alone and changes nothing for
anyone else.

Turning the switch back off leaves the roster where it is; it just stops moving worked stations
from then on.

### Step 4: Run the next round

Revisit stations in check-in order to take comments and traffic. Rounds are repeatable as many
times as the net needs, and prior notes are never lost when you come back around.

### Step 5: Capture notes

Record **per-station notes** on an individual entry through its
[detail modal](log-check-ins.md#step-7-open-the-detail-modal-for-the-full-record), and
**net-level notes** on the session as a whole.

A per-station entry has **two** note fields, and the difference matters:

- **Staff note (operators only)** — private to your logging team. It never appears on the live
  page, signed in or not, and never on the public stream.
- **Public note (everyone watching)** — shown on the roster row to every viewer, including
  someone with no account. Write it for the people following the net.

**Notes you wrote before the two fields existed are all staff notes.** Nothing you had already
written became public, and nothing was cleared — the field you have always typed into is the
private one, and it is now labelled that way. The public note starts empty on every entry.

The **net-level** note is a separate field again, and it stays operator-only.

All of them end up in the summary and the [exports](export-a-log.md).

### Step 6: Reorder when you need to

Reorder the roster when the working order should differ from check-in order — most often to run
[precedence](log-check-ins.md#step-8-set-precedence-when-it-matters) order on a traffic net.
Ordering by precedence works Emergency, then Priority, then Routine, preserving stable order
within each tier. With **Sink worked stations** on, that ordering happens inside each group.

## Verify

Open the session as a signed-out viewer in a second window. The working cursor sits on the same
station you selected, and moves within about two seconds when you move it.

With **Sink worked stations** on, that second window shows the same order you see: mark a
station worked on the console and watch it drop down the signed-out viewer's list too. The
order is shared; only the collapsed/expanded state of the worked group is per-viewer.

## What participants see

A participant whose entry is next up sees the your-turn indicator on their own row — a cyan left
bar, a **You** chip, and "You're next up". Which station that is follows the ordering you have
on. With **Sink worked stations** on, next up is the top of the unworked group — which can sit
above the working station once you come back to a station in a later round. With manual
ordering, next up is the first unworked station below the working one; a station above it was
passed on purpose. Account-less viewers see the working-station highlight and where they would
fall in the order.

Everyone watching — signed in or not — also sees, per station: the **staying status**, the
**precedence**, the **traffic count**, and the **public note**. That is the same set on the
operator console, on the live page with an account, and on the live page with none. Someone
following a traffic net can now see which station is holding emergency traffic rather than only
where it sits in the order.

They do **not** see the signal report, the operator's name, location or grid, corrections, who
logged the entry, or the staff note. Watching is still read-only: no viewer without the right
role gets an edit control, on any surface.

## Related guides

- [Log check-ins](log-check-ins.md)
- [Hand off net control](hand-off-net-control.md)
- [Export a log](export-a-log.md)
- [Live updates](live-updates.md) — why viewers see your cursor move without refreshing
