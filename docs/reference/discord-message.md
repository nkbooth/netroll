---
title: "Discord message"
description: "The Discord announcement NetRoll posts when a session closes — the embed shape, its size caps, the link back, rate-limit handling, and why it carries no signature and no roster."
docType: "reference"
audience: ["operator", "contributor"]
lastVerified: "2026-08-30"
---

# Discord message

The exact request NetRoll sends to a net's configured **Discord** webhook URL when a session of
that net closes. This is a different contract from the
[generic webhook payload](webhook-payload.md): that page describes a machine-readable,
HMAC-signed JSON document written for a receiver you build. This page describes a
**presentation-shaped message** a Discord channel renders and a person reads.

To set a Discord webhook URL, see [configure delivery](../nets/configure-delivery.md).

## The Discord webhook URL is a credential

A Discord webhook URL looks like `https://discord.com/api/webhooks/{id}/{token}` — the bearer
token is in the URL **path**. Anyone who holds the URL can post to that channel indefinitely.
Treat it exactly as you would a password:

- Do not paste it into a chat, an issue, or a screenshot.
- NetRoll stores it **recoverably** (as plain text in the database), because it has to send it
  on every delivery. That is the same posture as the generic webhook's signing secret stored
  beside it, and it is a known, tracked limitation rather than an oversight — see
  [How NetRoll stores it](#how-netroll-stores-it).
- **Rotation:** there is no rotate button in NetRoll, and there does not need to be. Delete the
  webhook in Discord (Server settings → Integrations → Webhooks) and create a new one, then
  paste the new URL into the net's delivery configuration. The old URL stops working the moment
  Discord deletes it.

## Request

| Property | Value |
|----------|-------|
| Method | `POST` |
| URL | exactly the URL you configured, verbatim |
| `content-type` | `application/json` |

**There are no other headers.** In particular there is **no `X-NetRoll-Signature` and no
`X-NetRoll-Delivery-Id`**. The [signature contract](webhook-payload.md#verifying-the-signature)
documented for the generic webhook **does not cover this message**: Discord verifies nothing, and
the webhook URL carries its own token, so a signature would secure nothing and imply a
verification step that does not exist.

## Body

One message, one embed, no `content` field:

```json
{
  "embeds": [
    {
      "title": "Sunday Traffic Net",
      "url": "https://netroll.example.radio/live/0192abcd-0000-7000-8000-000000000000",
      "description": "This net's session has closed. Full log: https://netroll.example.radio/live/0192abcd-0000-7000-8000-000000000000",
      "fields": [
        { "name": "Ways in", "value": "hf — 14.250 MHz\necholink — 12345", "inline": true },
        { "name": "Check-ins", "value": "12", "inline": true },
        { "name": "Started", "value": "2026-08-27T18:00:00+00:00", "inline": true },
        { "name": "Duration", "value": "3600 seconds", "inline": true }
      ],
      "footer": { "text": "Posted by NetRoll by N1CCK" },
      "timestamp": "2026-08-27T19:00:00+00:00"
    }
  ]
}
```

`Ways in` lists every way the net was reachable, one per line, in the owner's order — the
frequency for a radio way, the node or talkgroup for an internet-linked one. It replaced the
separate `Frequency` and `Band / mode` fields on 2026-08-30, when a net stopped having exactly
one frequency: a cross-mode net has several, and an internet-only net has none. A reader parsing
this message for a frequency should use the [generic webhook](webhook-payload.md) instead, whose
`connections` array is the machine-readable answer.

Three slots are conditional, and each on a different fact:

- `Started` appears only when the session has a recorded start instant.
- `Duration` appears only when it has **both** a start and a close instant — it is the difference
  between them, so a missing close removes it just as a missing start does.
- `timestamp` (the embed's own time, which Discord renders in the reader's local timezone) appears
  only when the session has a recorded close instant.

A session closed normally has all three. A session whose start or close was never recorded — an
older row, or one repaired by hand — is missing whichever of them depends on the absent instant.

## It carries no roster, by design

The announcement carries a **count**, a window, the ways in, and a link back. It never carries
per-station data — not a truncated roster, not "the first ten callsigns", not a `fields` entry
per check-in.

That is a deliberate product decision, not a size compromise. A chat channel is a place people
read, not a place software parses: the useful thing to post there is "this net closed, here is
where to look", and the full record is one click away at the linked session view. It also means
nothing in the message scales with how large the net was, so a 200-station net produces exactly
the same shaped message as a 3-station one.

If you need the roster itself, use the [generic webhook](webhook-payload.md) (every field, signed)
or the [CSV and ADIF exports](../live-sessions/export-a-log.md).

## The link back

The link is `{public base URL}/live/{sessionId}` — the public, account-less live-session view.
Two things worth knowing before you post it into a channel:

- A **closed** session still resolves there, which is why the link works at all.
- That view is the **redacted public** projection. It shows callsigns, list position, who is
  staying for comments, each station's precedence and traffic count, and the **public note** an
  operator wrote for people watching. It does **not** show signal reports, names, locations, grid
  squares, correction history, the **staff note**, or the entering operator's identity. Posting
  the link does not expose anything a signed-out visitor could not already see.

Anyone who can read the channel can open the link. If your net's participation should not be
public, do not post announcements into a public channel.

## Size limits

Discord caps embeds: 256 characters of `title`, 4096 of `description`, 25 fields, 256/1024 per
field name/value, 2048 of `footer.text`, and **6000 characters combined across every embed in one
message**. Breaking any of them returns `400` with `code: 50035`.

NetRoll declares its own, much smaller caps and enforces those, so the Discord limits are never
approached:

| Slot | NetRoll cap | Discord's limit |
|------|-------------|-----------------|
| `title` | 120 | 256 |
| `description` | 512 | 4096 |
| number of fields | 8 | 25 |
| `field.name` | 32 | 256 |
| `field.value` (every field except `Ways in`) | 64 | 1024 |
| `field.value` of `Ways in` | 1024 | 1024 |
| `footer.text` | 128 | 2048 |
| combined | **2488 maximum** | 6000 |

Every text slot is clamped by **code point** (not byte), and a clamped value ends with `…` so a
reader can see it was cut.

**`Ways in` is the exception, and it is shortened differently.** It is the one slot whose content
is a list rather than a scalar, so the scalar cap of 64 did not fit it and a mid-word `…` would
have read as the end of the list — a reader could not tell that a way to reach the net had been
dropped, let alone how many. It carries Discord's own limit as its cap, and when a net has more
ways than fit, the cut is by **whole line** and the last line says how many were not shown
(`+3 more — see the net page`). No way to reach a net is ever removed without saying so.

**The link is never partially cut.** Inside the description, the *prose* is the clamped part: it
shrinks around the link so your route to the full record survives. The one case that cannot work
that way is a `PUBLIC_BASE_URL` so long that the link alone will not fit the 512-character
description — around 470 characters, which no ordinary instance has. NetRoll then drops the link
from the description entirely rather than posting a broken one, because a truncated URL in a chat
channel is worse than none: the whole link is still on `url`, which is never clamped and which
Discord does not count toward the 6000.

**The field count is checked, not clamped.** The field list is at most five entries by
construction, and nothing in it scales with roster size, so NetRoll does not truncate it — the cap
of 8 is a bound the message is *tested against*, and an edit that made the list grow with the
roster fails that test instead of being silently trimmed to 8.

## Delivery behaviour

| Discord's response | What NetRoll does |
|--------------------|-------------------|
| `2xx` | Success. Done. |
| `429` (rate limited) | Transient. Waits the `retry_after` from the response body — up to **5 seconds** — then retries within the attempt budget. |
| `429` with `retry_after` **above 5 seconds** (including an absurd value like `1e30`, which no duration can hold) | Abandons this try now, without waiting. The announcement is rescheduled for the wait Discord asked for — not the ladder's shorter step — so NetRoll stays out of the window Discord closed. An absurd value is capped at 30 minutes. |
| `429` with a body that is not usable (HTML from a proxy, no `retry_after`, a value ≤ 0) | Still transient: waits the short default backoff and retries. |
| `5xx` | Transient. Retries after the short default backoff. |
| `404` | Permanent, never retried. Discord's own guidance: a webhook returning 404 must not be used again, and repeated attempts earn a temporary restriction. Recreate the webhook and paste the new URL. |
| `401`, `400`, any other `4xx` | Permanent, never retried. |
| Blocked address, non-HTTPS, invalid URL, too many redirects, oversized response | Permanent, never retried. |

Up to three attempts within one try at the announcement. If all three were transient, the
announcement is recorded as still owed and retried on a longer ladder — 30 seconds, doubling to
30 minutes, up to 10 tries in all — so a channel that was unreachable for a while still gets its
announcement, minutes late. NetRoll caps how long it will wait on a rate limit *within* a try
because a Discord announcement is worth sending promptly — a per-channel rate limit clears in well
under a second, so a multi-second wait means a global limit where waiting is unlikely to help, and
the delivery task is holding resources while it sleeps.

**One case is deliberately not retried.** If NetRoll restarts while a POST to Discord is in
flight, it cannot know whether the announcement landed: Discord's webhook API offers no
idempotency key, and confirming a post would mean reading channel history with a bot token this
instance does not have. Re-posting would risk a duplicate announcement in the channel, which is
worse than a missing one, so an announcement interrupted mid-POST is recorded as failed and not
sent again. The ambiguity is what forbids the retry, not the destination: NetRoll records the
moment it dispatches a POST, so an announcement whose request had not yet left the process when
the restart happened carries no ambiguity at all and is sent normally. The generic webhook and the summary email each carry a stable identifier (the
delivery id, the `Message-ID`) that lets a receiver recognise a repeat, so those two ARE retried
after a restart.

The URL goes through the same SSRF-safe outbound client every other user-supplied URL goes
through, with **no exemption for `discord.com`**: the host is re-resolved and re-checked against
the egress rules on every attempt.

## How NetRoll stores it

The Discord webhook URL is stored as plain text in the `net_delivery_configs` table, alongside
the generic webhook's signing secret, which is stored the same way. NetRoll must be able to read
both back in order to deliver, and this instance's threat model already accepts that database
access compromises stored credentials.

The practical consequence: **if your instance's database is ever exposed, treat every configured
Discord webhook as compromised** — delete those webhooks in Discord and create new ones.
Encrypting these values at rest is tracked work that has not been done.

## See also

- [Configure delivery](../nets/configure-delivery.md) — setting the Discord URL and the other targets
- [Webhook payload](webhook-payload.md) — the *other* delivery contract: signed, machine-readable, full roster
- [HTTP API](http-api.md) — the delivery-config endpoints
