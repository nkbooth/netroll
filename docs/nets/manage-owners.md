---
title: "Manage owners"
description: "Add and remove co-owners on a NetRoll net so it survives beyond a single account, and understand what happens to a net when an owner deletes their account."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-07-31"
---

# Manage owners

Share ownership of a net with other operators. Every owner has equal authority over the
definition, and a co-owned net outlives any one account.

## Prerequisites

- [A net definition you own](create-a-net.md).
- The account you want to add must already exist on this instance.
- Room under the instance's owner cap — the default is 5 owners per net. See
  [`MAX_OWNERS_PER_NET`](../reference/configuration.md#resource-caps).

## Steps

### Step 1: Open the net's owners

Open your net and go to its owners.

### Step 2: Add a co-owner

Add the account. The co-owner immediately gains equal edit and delete authority over the
definition — ownership is not tiered.

### Step 3: Remove a co-owner

Remove the account from the owner set. Removal takes their definition authority away; it does
not touch check-ins they logged in past sessions, which stay attributed to them.

## Verify

Sign in as the co-owner, or ask them to. They should be able to open the net's edit form and
save a change. A non-owner attempting the same request gets HTTP 403 from the server, whatever
the UI offers.

## What happens when an owner deletes their account

Account deletion is honored end-to-end, and the net's fate depends on whether anyone else owns
it:

| Situation | Result |
|-----------|--------|
| Co-owned net, one owner deletes | The net survives under the remaining owners. Only the departing account leaves the owner set. |
| Solely-owned net, the owner deletes | The net is archived — no longer listed and no longer live-runnable. It isn't hard-deleted, so past sessions stay attributable. |

Adding a co-owner is the way to make sure a club net doesn't get archived when one person
leaves.

## Related guides

- [Create a net](create-a-net.md)
- [Grant roles and relays](../live-sessions/grant-roles-and-relays.md) — session-scoped roles,
  which are a separate axis from ownership
- [Roles and permissions](../reference/roles-and-permissions.md)
- [Export or delete your data](../account/export-or-delete-your-data.md)
