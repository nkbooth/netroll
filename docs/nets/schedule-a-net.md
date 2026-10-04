---
title: "Schedule a net"
description: "Put a NetRoll net on a one-off or recurring schedule so its upcoming occurrences appear in discovery, with timezone-aware times shown in each viewer's local time."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-07-31"
---

# Schedule a net

Give your net a schedule so operators can find it before it runs. Scheduling generates
*occurrences* — the individual dated instances that appear in the upcoming list.

## Prerequisites

- [A net definition you own](create-a-net.md).

## Steps

### Step 1: Open the schedule

Open your net and go to its schedule.

### Step 2: Choose one-off or recurring

For a net that runs once, set a single start time. NetRoll records one upcoming occurrence.

For a net that runs regularly, choose daily, weekly, or monthly recurrence. NetRoll generates
upcoming occurrences from the pattern.

### Step 3: Set the timezone

Enter the start time with its timezone. Schedule in the timezone the net is *run* in, not the
one you happen to be sitting in — that way the net stays correct across daylight-saving
changes.

Every viewer sees the occurrence in their own local time, rendered relatively ("in 2h",
"40m ago").

### Step 4: Save

Save the schedule. Occurrences appear in the upcoming list on the discovery landing page,
provided the net is [Listed](net-visibility.md).

## Verify

Open the discovery landing page and find your net in the upcoming list. The time shown should
be your own local equivalent of the time you entered.

## Troubleshooting

**Duplicate occurrences.** The occurrence-generation job is idempotent — it's unique on the
combination of definition and scheduled start time, so re-running it never creates a duplicate.
If you see two occurrences at the same time, they came from two schedule entries, not from the
job.

**An edit didn't move a running session.** That's the definition-to-session boundary working as
designed. A session that has already started renders from its own snapshot; only future
occurrences pick up the edit. See [create a net](create-a-net.md#editing-a-net-later).

## Related guides

- [Create a net](create-a-net.md)
- [Net visibility](net-visibility.md)
- [Start and close a session](../live-sessions/start-and-close-a-session.md)
