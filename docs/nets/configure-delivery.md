---
title: "Configure delivery"
description: "Set per-net email, webhook and Discord targets so NetRoll delivers the session summary automatically when a net closes, with HMAC-signed webhooks and SSRF-safe egress."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-08-27"
---

# Configure delivery

Have NetRoll send the session summary somewhere the moment a net closes. Delivery is per net,
optional, and off until you configure a target.

## Prerequisites

- [A net definition you own](create-a-net.md). Only owners can change delivery targets.
- For a generic webhook: an HTTPS endpoint on a publicly-routable address that can verify an HMAC
  signature.
- For Discord: permission to manage webhooks in the target channel's server, so you can create
  one and copy its URL. Discord verifies nothing and there is no signature to implement.

## Steps

### Step 1: Open the net's delivery configuration

Open your net and go to its delivery configuration.

### Step 2: Add email targets

Add zero or more email addresses. Each one receives the session summary when a session of this
net closes.

### Step 3: Add a generic webhook target

Add an HTTPS URL. This is the machine-readable destination: NetRoll POSTs a signed JSON payload
carrying the whole roster. NetRoll validates it against the egress safety rules when you save it, and
rejects a URL it won't be allowed to call:

- HTTP is refused — HTTPS only.
- Hosts resolving to loopback, link-local, private (RFC 1918 / ULA), or cloud-metadata
  addresses are refused.

The same checks run again at delivery time against the pinned resolved IP, so a hostname that
passes at save time and re-resolves to a private address later is still blocked.

### Step 4: Add a Discord target

In Discord, open **Server settings → Integrations → Webhooks**, create a webhook for the channel
you want the announcement in, and copy its URL. Paste that URL into the net's **Discord webhook
URL** field.

**That URL is a credential — treat it like a password.** Its bearer token is in the URL itself, so
anyone who has it can post to that channel indefinitely. If it leaks, delete the webhook in
Discord and create a new one; there is nothing to rotate on the NetRoll side. See
[Discord message](../reference/discord-message.md) for the full posture and for what the
announcement contains.

NetRoll posts a short announcement — the net's name, the frequency, the window, the number of
check-ins, and a link back to the session — and never the roster itself.

### Step 5: Save

Save the configuration. With no email addresses, no webhook and no Discord URL, no delivery is
attempted — the on-screen summary and the
[CSV and ADIF downloads](../live-sessions/export-a-log.md) are still available.

## Verify

Close a session of this net. Each configured address receives the summary; your webhook endpoint
receives a POST carrying an HMAC signature you can verify against the payload; and your Discord
channel shows the announcement embed. The
[webhook payload reference](../reference/webhook-payload.md) documents every field of the signed
request and shows a full example, and the
[Discord message reference](../reference/discord-message.md) does the same for the announcement.

## How delivery behaves

Delivery runs at-least-once with an idempotency key, so a retry after a transient failure does
not double-deliver. Every configured target is delivered to independently, and a target that
fails permanently is recorded as failed without blocking the others — one dead webhook doesn't
stop the emails, and a rate-limited Discord channel doesn't stop the webhook.

Delivery is also durable. The moment a session closes, NetRoll records what it owes to each target
in the database, in the same write that closes the session, and the sends run in the background.
A target that is down for longer than the first quick retries cover is retried on a slower schedule
for about two hours — so after an outage a summary may arrive minutes late rather than not at all
— and a delivery interrupted by a NetRoll restart is picked up when the instance comes back. Email
retries carry a stable message id so a mail client that already has the summary can recognise a
repeat (most do; none is obliged to), and the webhook carries its delivery id for the same reason.
The one exception is a Discord announcement that was mid-post when the instance restarted: NetRoll
cannot tell whether it landed and will not risk posting it twice, so that one announcement is
recorded as failed rather than resent.

The three targets carry deliberately different things: the email is a human summary with the full
log attached as CSV and ADIF, the generic webhook is a signed machine-readable payload with every
roster field, and the Discord announcement is a short rendered message with a link back and no
roster at all.

## Troubleshooting

**A webhook URL was rejected on save.** It's either not HTTPS, or its hostname resolves into a
denied range. Internal hostnames and private IPs are refused by design; NetRoll won't let a
user-supplied URL reach an instance's internal network. The same rules apply to the Discord URL —
there is no exemption for `discord.com`.

**The Discord announcement stopped arriving.** A deleted webhook returns `404`, and NetRoll stops
using it immediately rather than retrying — Discord temporarily restricts callers that keep
hammering a dead webhook. Create a new webhook in Discord and paste the new URL.

**Nothing was delivered.** Delivery fires on session *close*. A session that was auto-closed by
the abandoned-session job delivers the same way; a session still live has not delivered yet. If
the session is closed and a target was unreachable, wait: retries continue for about two hours,
and a summary that arrives late was not lost. A target that has been down for longer than that, or
that refused the delivery outright (a deleted Discord webhook, an endpoint answering `4xx`), is
recorded as failed and not retried — fix the target, and the next session's close delivers
normally.

## Related guides

- [Webhook payload reference](../reference/webhook-payload.md) — the exact request your endpoint
  receives, and how to verify its signature
- [Discord message reference](../reference/discord-message.md) — the announcement embed, its caps,
  and why it is not signed
- [Export a log](../live-sessions/export-a-log.md)
- [Start and close a session](../live-sessions/start-and-close-a-session.md)
- [Configuration reference](../reference/configuration.md) — SMTP settings the instance needs
  for email delivery to work at all
