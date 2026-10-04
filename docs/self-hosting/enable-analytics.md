---
title: "Enable analytics"
description: "Turn on NetRoll's optional cookie-less Plausible analytics against your own Plausible instance, or confirm that a default instance loads no tracker at all."
docType: "howto"
audience: ["self-hoster"]
lastVerified: "2026-09-13"
---

# Enable analytics

Point your instance at a Plausible server so you can see traffic. Analytics is off by default: a
NetRoll instance with nothing configured never downloads the tracker and requests nothing from
any external host.

## Prerequisites

- [A running instance](quickstart.md).
- A Plausible instance you control, or a Plausible Cloud account.
- A site registered in that Plausible instance whose domain matches exactly what you're about to
  configure. Events for an unregistered domain are rejected.

## Steps

### Step 1: Set the reported domain

Set `PLAUSIBLE_DOMAIN` to the site domain this instance reports as:

```bash
PLAUSIBLE_DOMAIN=netroll.example.com
```

This value is the on/off switch. Empty or unset means no analytics at all.

### Step 2: Point at your Plausible instance

Set `PLAUSIBLE_SCRIPT_HOST` to the origin of your self-hosted Plausible:

```bash
PLAUSIBLE_SCRIPT_HOST=https://analytics.example.com
```

Events are posted to `<origin>/api/event`. Leave it unset to use Plausible's own cloud host.
It's ignored entirely when `PLAUSIBLE_DOMAIN` is unset — a host with nothing to report as
receives nothing.

### Step 3: Restart

```bash
podman compose up -d
```

### Step 4: Check your reverse proxy sets no Content Security Policy

NetRoll's own CSP allows `connect-src` to the origin you configured in Step 2 — or to Plausible's
cloud host when you left it unset — automatically, so there is nothing to add. If your reverse
proxy sets a CSP of its own, remove it: two CSP headers are enforced together, and a proxy policy
that does not name your Plausible origin blocks every event. The tracker itself needs no
`script-src` exception — it ships inside NetRoll's own bundle.

## Verify

Load the app with your browser's network tab open. You should see requests to
`<your-plausible-origin>/api/event` and no script fetched from any external host.

With analytics unset, you should see neither: no tracker chunk downloaded, and no outbound
request.

## What NetRoll sends

The tracker ships in NetRoll's own bundle rather than being fetched from Plausible, so enabling
analytics loads no third-party script. It's cookie-less and collects no personal data. The only
external host involved is the Plausible instance you name.

## Related guides

- [Configuration reference](../reference/configuration.md#analytics)
- [Deployment model](deployment-model.md)
- [Self-hosting quickstart](quickstart.md)
