---
title: "WebSocket protocol"
description: "NetRoll's session WebSocket wire format — the snapshot and delta frames, the since-seq resume cursor, event kinds, and how the public stream is redacted."
docType: "reference"
audience: ["contributor"]
lastVerified: "2026-09-14"
---

# WebSocket protocol

The wire format for NetRoll's live session streams. A client connects, receives either a
snapshot or a replay, then receives deltas in ascending `seq` for as long as it stays connected.

For why the protocol is shaped this way, see [live updates](../live-sessions/live-updates.md)
and [ARCHITECTURE.md](https://github.com/nkbooth/netroll/blob/main/ARCHITECTURE.md).

## Endpoints

| Path | Access | Stream |
|------|--------|--------|
| `/api/net-sessions/{id}/ws` | Session cookie; `ViewConsole` capability on the session | Full snapshot and deltas, plus ephemeral lock frames |
| `/api/net-sessions/{id}/live/ws` | None — the session id in the URL is the read capability | Redacted snapshot and deltas; never emits lock frames |

Both check the `Origin` header against the instance's `PUBLIC_BASE_URL` before any database
work, as defense in depth against cross-site WebSocket hijacking. A request with a mismatched
origin is refused with HTTP 403. A missing `Origin` is allowed — non-browser clients don't send
one.

## Connect parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `since` | integer | `0` | The `seq` the client last folded. |

Behaviour depends on the value:

- **`since` absent or `0`** — fresh connect. The server sends one `snapshot` frame first, then
  live deltas.
- **`since` > 0** — resume. No snapshot. The server replays events with `seq` greater than
  `since` in order, then continues with live deltas.

An invalid `since` closes the connection with code `1008`.

### Close reasons

| Code | Reason text | When |
|------|-------------|------|
| `1008` | `since exceeds the session's latest sequence` | The resume cursor is beyond the session's newest event. Re-connect without `since`. |
| `1008` | `this net's log predates its connection list and can no longer be opened` | The session was logged before a net could list more than one way to reach it. **Permanent — do not reconnect.** The authenticated stream never reaches this: it authorizes before the upgrade and answers `410 /errors/unreplayable-log` in problem+json instead, so only the public stream closes this way. |

Every other failure closes the socket without a reason, because the client recovers from all of
them by reconnecting.

## Server frames

Every frame carries a `type` discriminator.

### `snapshot`

Sent exactly once, first, on a fresh connect.

```json
{
  "type": "snapshot",
  "session": { "...": "folded session summary", "latestSeq": 42 }
}
```

The `session` object is byte-shape-identical to the body of
`GET /api/net-sessions/{id}`, including `viewerRole`. The client's resume cursor is
`session.latestSeq`.

On the public stream, `session` is the redacted public view instead.

### `event`

One appended event delta. The event element is flattened into the frame.

```json
{
  "type": "event",
  "seq": 43,
  "kind": "checkin.added",
  "actorId": "0192f3c1-...",
  "at": "2026-07-31T18:04:11Z",
  "payload": { "...": "variant fields, camelCase" }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `seq` | integer | Per-session monotonic sequence number. Ascending, no gaps, no duplicates. |
| `kind` | string | Stable `noun.verb` token. |
| `actorId` | UUID | The account that caused the event. **Omitted**, not null, when absent. Always omitted on the public stream. |
| `at` | string | RFC 3339 UTC timestamp. |
| `payload` | object | The variant's own fields. Byte-identical to the stored event payload. |

The same element is served bare, as array members, by the HTTP catch-up endpoints — so a client
folds one event shape whether a delta arrived over the socket or over HTTP.

### `lock`

An ephemeral soft-lock advisory. It carries **no `seq`**, is never folded into session state,
and updates a separate ephemeral slice of the client store.

```json
{
  "type": "lock",
  "checkInId": "0192f3c1-...",
  "holderCallsign": "KC1ABC",
  "expiresAt": "2026-07-31T18:04:26Z"
}
```

`holderCallsign` and `expiresAt` are both `null` on release, meaning the entry is free.

Emitted on the authenticated stream only. The public stream never emits it.

## Event kinds

| Kind | Meaning |
|------|---------|
| `session.started` | The session went live. |
| `session.closed` | The session closed. No further mutations accepted. |
| `session.note-set` | The net-level note changed. |
| `frequency.changed` | ONE way in moved to a new frequency. Carries `connectionId` naming which — a net can be reachable several ways at once, so a frequency with no connection beside it is unattributable. Added 2026-08-30. |
| `checkin.added` | A station was checked in. |
| `checkin.updated` | An entry was edited. |
| `checkin.removed` | An entry was removed. |
| `roster.reordered` | The roster order changed. |
| `roster.order-mode-set` | The roster ordering mode changed (`manual` or `worked-sink`). Redacted to `{}` on the public stream. |
| `station.worked-set` | The working-station cursor moved. At most one station at a time. |
| `station.blocked` | An account was blocked from the session. |
| `ncs.stalled` | Net control's heartbeat was missed. The session is paused. |
| `ncs.resumed` | Net control returned or control was claimed. |
| `control.handed-off` | Net control transferred to another operator. |

## Redaction on the public stream

The public stream projects an **allowlist** of public-safe payload keys per event kind. A field
is public only if it is explicitly projected — nothing leaks by default.

A newly added event **variant** fails to compile until its public projection is declared. A new
**field on an existing variant** does not, and once did not at all: the two check-in arms
ended in a rest pattern, so a field added to `checkin.added` compiled green and was silently
redacted. Both arms now name every field they drop, so a new one is a compile error a person has
to answer — but the guarantee is a code convention, not a property of the type system, and it
holds only for as long as those arms stay exhaustive.

| Kept | Omitted |
|------|---------|
| A moved way in's `connectionId` and its new frequency | `actorId` on every frame |
| Callsign and the check-in row id | `clientEventId` |
| Check-in `source` — staff or self | `name`, `location`, `signalReport` |
| `staying` on both check-in frames | The staff note on a check-in |
| `via` on both check-in frames, structured, when recorded | Corrections and operator ids |
| | `relayedBy` on both check-in frames — see below |
| `precedence`, `traffic` and the public note on `checkin.updated` | Which account holds control |
| Roster order from a reorder | The net-level note's contents |
| Working-station cursor position | |
| The fact that control changed | |
| The fact that a net-level note changed | |

The stalled and resumed kinds are public: a paused net is visibly observable because the roster
freezes. Their payloads are empty — the fold derives control state from the kind alone.

`session.started`'s public payload is empty too, and that was decided rather than inherited: what
remains on it is the definition id and version, which are internal provenance, not public radio
data. The ways the net is reachable ride the snapshot, so a public subscriber that sees this
frame refetches rather than reading it. In practice no public subscriber ever sees it —
`session.started` is a session's first event, and the only `since` value whose replay would
include it is `0`, which is the fresh-connect branch that sends a snapshot instead.

A `roster.reordered` frame carries the order and nothing per-station, so it has no precedence
label to withhold. The labels reach observers on `checkin.updated`.

The two check-in frames carry **different** field sets, because the two events do:

- `checkin.added` carries `checkInId`, `callsign`, `source` and `staying` always, plus `via` when
  the operator recorded which way in the station arrived on. Precedence, the traffic count and
  both notes are **edit-only fields**: a check-in event has no place to put them, so there is
  nothing to project. Do not expect a precedence label on an add; read it from the snapshot, or
  wait for the first `checkin.updated`. `via` is the one field that is set **at add** and
  therefore appears here as well as on the update — that is why it is on this frame and the four
  edit-only fields are not.
- `checkin.updated` carries `checkInId`, `callsign`, `staying` and `precedence` always, plus
  `traffic`, `publicNote` and `via` when the operator has set them. Every optional follows the
  same convention `actorId` does in the frame table above — **omitted**, not null, when absent —
  so a client must test for the key's presence, never for a null value.

On the **authenticated** stream, both check-in frames also carry `relayedBy` when the operator
recorded which station passed that station's traffic. Like `via` it is set **at add** and
editable afterwards, so it rides both frames rather than the update alone; unlike `via` it is a
plain callsign string with no label beside it, because a callsign is already the text a person
reads. Added 2026-09-02.

`relayedBy` is **never** on the public stream, and that is a decision rather than an oversight.
`via` names a connection the net owner published — the same page already lists it, and the public
`frequency.changed` delta already names its id. `relayedBy` names a third-party station that never
checked in and appears nowhere else on that page, which puts it with the operator account id the
public stream redacts, not with `via`. An observer watching a public roster wants to know how to
reach the net, not who passed the traffic.

`via` is a `kind`-discriminated object, never two nullable keys:
`{"kind":"connection","connectionId":"…"}` names one of the connections in the session snapshot
by its stable id, and `{"kind":"unlisted","text":"…"}` carries the operator's own words for a way
the net does not list. The public stream carries the **structured** value and no label — resolve
it against the snapshot's `connections`, which the same stream already publishes. That resolution
yields the connection's **current** frequency; the stream carries no per-check-in frequency, so a
station checked in before a mid-session move reads as being on the frequency the net is on now,
not the one it was worked on. An absent `via`
means *nobody recorded one*, which is a different fact from *arrived on the net's first
connection*; never substitute one for the other.

`relayedBy` and `via` are **two different facts** and neither substitutes for the other: one says
how the traffic travelled, the other says who passed it. A station may carry both, either, or
neither, and an edit that changes one never touches the other. `relayedBy` is also distinct from
the entry's own entering operator, which names the NetRoll account that typed it.

Per-station notes are **two** fields. The **public note** is written for everyone watching and
rides the public stream; the **staff note** is operator-private and rides no public frame. The
persisted key for the staff note is still `notes`, so every event written before the split
decodes into the private field.

## Constraints

- Deltas arrive in ascending `seq`. A client that folds them in order and tracks the highest
  `seq` can always resume with no gaps and no duplicates.
- Applying the same event twice never duplicates a roster entry, so a retried write is safe.
- The propagation target is about two seconds at the 95th percentile, measured server-commit to
  client-receipt at this layer. It excludes device rendering.
- Postgres is the ordering authority. The pub/sub layer only notifies that an event landed; if
  it's unavailable, clients recover through the `?since=` resume path.
- Account-less clients get a read-only stream with no write capability.

## See also

- [HTTP API](http-api.md) — the `?since=` catch-up endpoints and every mutation
- [Live updates](../live-sessions/live-updates.md) — the same mechanism for a non-integrator
- [ARCHITECTURE.md](https://github.com/nkbooth/netroll/blob/main/ARCHITECTURE.md) on GitHub
