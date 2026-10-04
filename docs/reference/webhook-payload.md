---
title: "Webhook payload"
description: "The on-close webhook contract — request headers, the HMAC signature, the JSON payload fields, and a full worked example."
docType: "reference"
audience: ["operator", "contributor"]
lastVerified: "2026-09-04"
---

# Webhook payload

The exact request NetRoll sends to a net's configured **generic webhook** URL when a session of
that net closes. This page is the contract for anyone writing a receiver.

A net can also have a **Discord** destination, which is a different contract with a different
shape and no signature — see [Discord message](discord-message.md). Nothing on this page
describes that message.

To set a webhook URL, see [configure delivery](../nets/configure-delivery.md).

## Request

| Property | Value |
|----------|-------|
| Method | `POST` |
| URL | The net's configured webhook URL, verbatim |
| Body | UTF-8 JSON, compact (no insignificant whitespace) |
| Timeout | 15 seconds per attempt |
| Attempts | Up to 3, with the identical body and headers each time |

### Headers

| Header | Description |
|--------|-------------|
| `content-type` | Always `application/json`. |
| `X-NetRoll-Signature` | `sha256=` followed by 64 lowercase hex characters — the HMAC-SHA256 of the raw request body under the net's stored webhook secret. |
| `X-NetRoll-Delivery-Id` | `net-session-close:{sessionId}`. Stable across all retries of the same session. Dedupe on this value. |

### Verifying the signature

Compute `HMAC-SHA256(secret, raw_request_body)`, hex-encode it in lowercase, and prefix it with
`sha256=`. Compare against `X-NetRoll-Signature` in constant time.

Sign the **raw bytes as received**. Re-serializing the parsed JSON changes key order and
whitespace, which changes the digest.

## Payload fields

Keys are camelCase. Optional fields are **omitted entirely** when absent — they are never
`null`.

| Field | Type | Always present | Description |
|-------|------|----------------|-------------|
| `net.title` | string | Yes | The net title, from the definition snapshot frozen at session start. |
| `sessionId` | UUID | Yes | The closed session. |
| `definitionId` | UUID | Yes | The net definition the session ran from. |
| `definitionVersion` | integer | Yes | The definition version frozen at session start. |
| `connections` | array | Yes | Every way the net was reachable at close, in the owner's order, each with the frequency it ended on. Never empty. See [Connection fields](#connection-fields). |
| `startedAt` | timestamp | No | Omitted if the session never started. |
| `closedAt` | timestamp | No | Omitted if the close instant is unknown. |
| `durationSeconds` | integer | No | Whole seconds between `startedAt` and `closedAt`. Omitted unless both are present. Never negative. |
| `participantCount` | integer | Yes | Roster length. |
| `roster` | array | Yes | One object per station, in the session's final roster order. That is check-in order unless net control reordered the roster or ran the worked-sink ordering mode, in which case it is the order the session ended in. Each entry's `checkedInAt` still recovers check-in order. Empty for a session with no check-ins. |

### Connection fields

A net is reachable in one or more ways — an HF frequency, a repeater, an EchoLink node, a DMR
talkgroup — and `connections` carries all of them, in the order the net's owner arranged them.
`connections[0]` is the way the net leads with; it is **not** guaranteed to be a radio
frequency, because an internet-only net has none.

**These objects are the one exception to the omitted-not-null rule above.** A connection carries
every key below on every kind, with `null` where the key does not apply — `band` is `null` on an
EchoLink connection, `node` is `null` on an HF one. Key on `kind`, not on which keys are present.

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Stable identifier for this way in, frozen onto the session at start. |
| `position` | integer | Zero-based place in the owner's order. Dense, no gaps. |
| `kind` | enum | `hf`, `repeater`, `echolink`, `allstar`, `dmr`, `dstar`, `ysf`, `urf`, or `other`. Treat an unrecognised value as `other` rather than failing. |
| `plannedFrequencyHz` | integer \| null | Exact frequency in hertz, on `hf` and `repeater` only. This is the frequency the net **ended on**, including any mid-session move. |
| `band` | string \| null | Band token, for example `20m`. `hf` and `repeater` only. |
| `mode` | string \| null | Mode token, for example `ssb`. `hf` and `repeater` only. |
| `repeaterOffsetHz` | integer \| null | Signed repeater shift in hertz. |
| `toneMode` | string \| null | Tone mode token. |
| `toneValue` | string \| null | Tone value as text. |
| `node` | string \| null | EchoLink **or** AllStar node number — which one is decided by `kind` alone. |
| `reflector` | string \| null | D-Star, YSF or URF reflector — again disambiguated only by `kind`. |
| `network` | string \| null | The DMR network a talkgroup lives on. `null` on every other kind, and on a DMR connection whose network nobody recorded. |
| `talkgroup` | string \| null | DMR talkgroup id, as text. |
| `label` | string \| null | An `other` connection's short name. |
| `detail` | string \| null | An `other` connection's prose. |

### Roster entry fields

| Field | Type | Always present | Description |
|-------|------|----------------|-------------|
| `callsign` | string | Yes | The station callsign as logged. |
| `name` | string | No | The per-check-in operator name. |
| `location` | string | No | The per-check-in location, as free text. |
| `grid` | string | No | The per-check-in Maidenhead grid locator. **4, 6 or 8 characters** — an uppercase field pair `AA`–`RR`, two digits, an optional lowercase subsquare pair `aa`–`xx`, and an optional final two digits: `FN31`, `FN31pr`, `FN31pr47`. Match on `^[A-R]{2}[0-9]{2}([a-x]{2}([0-9]{2})?)?$` rather than on the length of any one example. Separate from `location`, not a formatted version of it: `location` is a place name a human reads, `grid` is a machine-meaningful locator. An operator may record either, both, or neither, so do not derive one from the other. |
| `source` | enum | Yes | `staff` — logged by net control or a logger. `self` — self-service check-in. |
| `enteringOperator` | string | No | The callsign of the staff member who logged the entry. Present only when `source` is `staff` and that account has a claimed callsign. Never present on a `self` entry. |
| `signalReport` | string | No | The signal report as logged, for example `59`. |
| `staying` | enum | Yes | `staying-for-comments` or `in-and-out`. |
| `precedence` | enum | Yes | `routine`, `priority`, or `emergency`. |
| `traffic` | integer | No | Traffic pieces held. Omitted when none. |
| `worked` | boolean | Yes | Whether net control has worked this station. |
| `notes` | string | No | The **staff** note on the entry — free text, private to the logging team on every NetRoll surface. The key is unchanged; only its meaning narrowed when the field split. |
| `publicNote` | string | No | The **public** note on the entry — free text shown to everyone watching the live page. Additive; omitted when none. |
| `via` | object | No | **Which way in this station arrived on.** A `kind`-discriminated object, never two nullable keys: `{"kind":"connection","connectionId":"…"}` names one of the `connections` above by its stable id, and `{"kind":"unlisted","text":"…"}` carries the operator's own words for a way the net does not list. Omitted when nobody recorded one — which is a **different fact** from "arrived on the net's first connection", and NetRoll never substitutes one for the other. |
| `relayedBy` | string | No | **Which station passed this station's traffic**, as its callsign. A *different* fact from `via` beside it: `via` says how the traffic travelled, this says who passed it, and a station may have one, both, or neither. Omitted when nobody relayed the station. There is deliberately no `relayedByLabel` — a callsign is already the text a person reads. It is also **not** `enteringOperator`, which names the NetRoll account that typed the entry. |
| `viaLabel` | string | No | The same `via`, resolved to the text a person reads (`HF — 14.230 MHz`, `EchoLink — 12345`). Present whenever `via` is. It is supplied so a receiver never has to render a connection id, and so every integration reads the same name for a connection. **The frequency in the label is the one that connection was on when this station checked in**, while `connections` reports where each way in **ended** — so for a station worked before a mid-session move, resolving `via` against `connections` yourself names the same connection but a different frequency. Read `viaLabel` for what the station was worked on. A `via` naming a connection this session's snapshot no longer holds resolves to a fixed phrase saying so, never to a blank and never to another connection's label. |
| `checkedInAt` | timestamp | Yes | When the station was added to the roster. |

### Timestamp format

RFC 3339 with an explicit `+00:00` offset rather than `Z`, for example
`2026-08-09T18:00:00+00:00`. A non-zero millisecond component renders as a fractional second
(`2026-08-09T18:00:00.472+00:00`), so parse with a full RFC 3339 parser rather than a fixed
format string.

## Compatibility

How this contract may change, and what a receiver has to tolerate.

**Fields may be added in any release.** A new key on the payload or on a roster
entry is an additive change: it ships whenever the field it carries ships, with no
announcement and no version bump of this contract.

> **Added 2026-08-30:** each roster entry may now carry `via` and `viaLabel` —
> which way in that station arrived on. Both are omitted when nobody recorded
> one, so a payload from a net that records no ways in is byte-identical to one
> sent before the fields existed. Nothing was removed and nothing changed shape.

> **Added 2026-09-02:** each roster entry may now carry `relayedBy` — the
> callsign of the station that passed that station's traffic. It is omitted when
> nobody relayed the station, so a payload from a net where nothing was relayed
> is byte-identical to one sent before the field existed. Nothing was removed and
> nothing changed shape. Note that it is a **separate fact** from `via`, which
> continues to record only how the traffic travelled.

> **Changed 2026-09-04:** the **value** of `viaLabel` changed for a station
> worked before a mid-session frequency move. No key was added or removed and
> nothing changed shape. The label now carries the frequency that connection
> was on **when that station checked in**; it used to carry the frequency the
> connection **ended** on, which is what `connections` still reports. For a net
> that never moved frequency the payload is byte-identical. A receiver that
> reconstructed a station's frequency by resolving `via` against `connections`
> should read `viaLabel` instead — the two now legitimately differ for pre-move
> stations, and the array is the one that describes the net rather than the
> station. Announced here because the two additive rules above do not cover a
> change of meaning.

**Ignore unknown keys.** A receiver MUST ignore any key it does not recognise, on
the payload object and on a roster entry alike. This is the rule that makes the
one above safe; a receiver that rejects or fails on an unrecognised key is outside
this contract.

Two more rules a receiver has to hold to are stated in full elsewhere in this
document and are deliberately not repeated here: the null-handling rule under
[Payload fields](#payload-fields), and the deduplication rule under
[Headers](#headers). The first is what makes an added optional field safe — a
payload that does not carry an optional field is byte-identical to one sent
before that field existed. The second is about retries, not about fields.

There is no payload version and none is planned. **Fields may be added** and
**ignore unknown keys** — the two rules stated in full above — are the whole of
the compatibility mechanism. `definitionVersion` is the **net definition's**
version, not the payload's, and must not be read as one.

### Consumer notice: three fields were REMOVED (2026-08-30)

Removal is not covered by the rules above, so it is announced here. A net can now
be reachable in several ways at once, or in no radio way at all, and three fields
that each named exactly one frequency could not survive that:

| Removed | Replaced by |
|---------|-------------|
| `net.band` | `connections[n].band`, per way in |
| `net.mode` | `connections[n].mode`, per way in |
| `operatingFrequencyHz` | `connections[n].plannedFrequencyHz`, per way in, already including any mid-session move |

A receiver that read any of those three must move to `connections`. There is no
translated stand-in, and none is planned: for an internet-only net the old fields
had no honest value, and for a cross-mode net they had several. **Do not fall back
to `connections[0]` as "the" frequency** — position zero is the way the owner leads
with, which need not be a radio way at all.

`net` still exists and still carries `title`.

## Example

Headers:

```http
POST /hooks/net-close HTTP/1.1
Host: hooks.example.com
content-type: application/json
X-NetRoll-Signature: sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8
X-NetRoll-Delivery-Id: net-session-close:3f8b1c2e-9a4d-4f61-b0d7-5c8e2a11d904
```

Body, pretty-printed here for readability. A real delivery sends it compact, and a real
signature covers those compact bytes — the header above is a placeholder, not the HMAC of this
body:

```json
{
  "net": {
    "title": "Sunday Traffic Net"
  },
  "sessionId": "3f8b1c2e-9a4d-4f61-b0d7-5c8e2a11d904",
  "definitionId": "c1d4e7a0-2b36-4c58-9e1f-77a0b3d6e512",
  "definitionVersion": 3,
  "connections": [
    {
      "id": "8f1d0a6c-3b2e-4a77-9c10-0e5b4d2f8a31",
      "position": 0,
      "kind": "hf",
      "plannedFrequencyHz": 14250000,
      "band": "20m",
      "mode": "ssb",
      "repeaterOffsetHz": null,
      "toneMode": null,
      "toneValue": null,
      "node": null,
      "reflector": null,
      "network": null,
      "talkgroup": null,
      "label": null,
      "detail": null
    },
    {
      "id": "2c9a7e14-56b8-4d03-b1f2-9a3c8e6d0071",
      "position": 1,
      "kind": "echolink",
      "plannedFrequencyHz": null,
      "band": null,
      "mode": null,
      "repeaterOffsetHz": null,
      "toneMode": null,
      "toneValue": null,
      "node": "12345",
      "reflector": null,
      "network": null,
      "talkgroup": null,
      "label": null,
      "detail": null
    }
  ],
  "startedAt": "2026-08-09T18:00:00+00:00",
  "closedAt": "2026-08-09T19:12:30+00:00",
  "durationSeconds": 4350,
  "participantCount": 3,
  "roster": [
    {
      "callsign": "N1CCK",
      "name": "Nick",
      "location": "Nashua NH",
      "grid": "FN42gt",
      "source": "self",
      "staying": "staying-for-comments",
      "precedence": "routine",
      "worked": true,
      "checkedInAt": "2026-08-09T18:01:12+00:00"
    },
    {
      "callsign": "W1AW",
      "name": "Maria",
      "location": "Newington CT",
      "grid": "FN31pr",
      "source": "staff",
      "enteringOperator": "K1OP",
      "signalReport": "59",
      "staying": "in-and-out",
      "precedence": "priority",
      "traffic": 2,
      "worked": true,
      "notes": "Two pieces for Hartford",
      "publicNote": "Relaying for W1BBB",
      "via": { "kind": "connection", "connectionId": "0192f4a1-0000-7000-8000-00000000c001" },
      "viaLabel": "HF — 14.230 MHz",
      "relayedBy": "W3REL",
      "checkedInAt": "2026-08-09T18:04:47+00:00"
    },
    {
      "callsign": "K2ABC/M",
      "source": "staff",
      "enteringOperator": "K1OP",
      "staying": "in-and-out",
      "precedence": "routine",
      "worked": false,
      "checkedInAt": "2026-08-09T18:22:03+00:00"
    }
  ]
}
```

The first entry is self-service, so it carries no `enteringOperator`. The third shows the
minimal shape: every optional field absent.

The signature above is illustrative. Verify by recomputing the HMAC over the body you received.

## Delivery behaviour

Delivery is spawned when the session closes and is never awaited, so a slow or unreachable
receiver cannot delay the close. This applies to both close paths: a manual close and an
auto-close by the abandoned-session sweep.

Retries run on two ladders. Within one try at the delivery, a transient failure is retried up to
3 times with a short backoff. If that budget is spent, the delivery is not abandoned: NetRoll
records it as still owed and retries on a longer ladder — 30 seconds, then 1, 2, 4, 8 and 16
minutes, then every 30 minutes — up to 10 tries in all, roughly two hours of outage. A delivery
interrupted by a NetRoll restart mid-send is resumed by the next process, because what is owed
is written to the database in the same transaction that closes the session.

That is why the delivery id matters. Your endpoint may receive the same delivery more than once —
after a restart, or after a slow response of yours was read as a failure — and every attempt
carries the identical `X-NetRoll-Delivery-Id` and the identical body bytes, however far apart in
time. Treat the id as the dedupe key; an endpoint that ignores it will process a summary twice.

The signature is stable too, with one exception worth knowing if you pin it: the secret is minted
once per webhook URL and dropped when the URL is cleared, so if the net's owner removes and re-adds
the webhook between the first attempt and a later retry, the retry is signed with a new secret.
Verify against the secret you currently hold rather than against a previously seen signature.

Response handling:

| Response | Behaviour |
|----------|-----------|
| `2xx` | Success. No further attempts. |
| `429` or `5xx` | Transient. Retried within the try, then rescheduled on the longer ladder until its 10 tries are spent. |
| Any other status, including `3xx` | Permanent. Recorded and abandoned without retry. |
| Timeout or transport failure | Transient. Retried. |
| Blocked address, non-HTTPS, invalid URL, too many redirects, oversized response | Permanent. Never retried — a blocked address at delivery time is a DNS rebind caught in the act. |

The target host is re-resolved and re-checked against the egress rules on every attempt, not
just when the URL was saved.

A net with a webhook URL but no stored secret delivers nothing and records the delivery as
failed. NetRoll never mints a replacement secret, because that would produce a signature the
receiver cannot verify.

## See also

- [Configure delivery](../nets/configure-delivery.md) — setting the URL and email targets
- [Discord message](discord-message.md) — the other delivery contract: a rendered announcement, not a signed payload
- [HTTP API](http-api.md) — the delivery-config endpoints
- [Export a log](../live-sessions/export-a-log.md) — the CSV and ADIF the email path attaches
