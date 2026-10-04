---
title: "Who can fix a net's details"
description: "Net data belongs to the net's owners. How an owner edits a net, how to reach an owner when you are not one, and when a change that will not save is a bug to report."
docType: "howto"
audience: ["participant", "operator"]
lastVerified: "2026-09-14"
---

# Who can fix a net's details

A net's time is wrong, its frequency has moved, or the description is out of date. Here is who
can change it.

## Net data belongs to the net's owners

The people who created a net edit it. The maintainer of the software and the operator of an
instance do not edit other people's nets, on the public instance or anywhere else: a net's
entry in the directory says what its owners say it says.

## If you own the net

Sign in, open **My Nets** from the header, find the net, and press **Edit**. The **Edit net**
page holds everything the directory shows: the title and description, the ways to reach the net,
the schedule, the owners, and the visibility. Press **Save changes**, and the directory reflects
it.

A change to the definition never disturbs a session already running. A live session works from
a copy of the definition taken when it started, so your edit shows on the next session, not the
one on the air right now.

- [Create a net](nets/create-a-net.md) describes every field on that page.
- [Schedule a net](nets/schedule-a-net.md) covers the schedule and the upcoming list.
- [Manage owners](nets/manage-owners.md) shows how to add a co-owner, so more than one person
  can fix things.

## If you do not own the net

NetRoll stores no contact details for a net's owners and relays no messages to them. Reach them
the way the net does: on the air at its listed time and way in, or through the club or group
that runs it. A net that has gone stale with no reachable owner is still its owners' net.

## If you own the net and the change will not save

That is a bug. [File it](https://github.com/nkbooth/netroll/issues/new?template=bug_report.yml)
with the net's title and what you tried. Callsigns and net names are public; leave out email
addresses and anything from a sign-in link.
