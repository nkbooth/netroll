---
title: "Glossary"
description: "Definitions of every term NetRoll uses — amateur-radio net vocabulary and the NetRoll-specific meanings of definition, session, occurrence, seq, and source badge."
docType: "reference"
audience: ["operator", "participant", "self-hoster", "contributor"]
lastVerified: "2026-09-14"
---

# Glossary

Terms NetRoll uses, including the ones that carry a specific meaning inside the app. Where a
term is ambiguous in general amateur-radio usage, the NetRoll meaning given here is the one the
documentation always uses.

Which of several candidate words the code and the docs actually use is fixed on the
[vocabulary](vocabulary.md) page; this one says what the words mean.

## Amateur radio

| Term | Definition |
|------|-----------|
| ADIF | Amateur Data Interchange Format. The file format logging programs exchange contacts in. NetRoll exports the net from the net-control perspective. |
| Band | The frequency band a net runs on. Stored as a structured value, used by discovery filters. |
| Callsign | An operator's assigned identifier. On NetRoll it's self-asserted and unverified against license records. |
| Check-in | One station's participation in a session. In NetRoll, one roster entry with a full record attached. |
| CTCSS / DCS | Repeater sub-audible tone systems. Recorded in a net's repeater details. |
| Grid | Maidenhead grid locator. Validated on input. |
| Mode | How the net operates — phone, CW, digital, FM, repeater. Shapes the signal-report format. |
| Net | A coordinated on-air meetup where stations check in and follow a net-control-moderated agenda. |
| NCS | Net Control Station. The operator running the net. A NetRoll session has exactly one active NCS. |
| Precedence | Message priority on a traffic net: Routine, Priority, or Emergency. |
| Relay | A station that passes check-ins for operators net control can't hear. **The word means three distinct things in NetRoll — see the three rows below.** |
| Relay (the act) | What happens on the air: one station hears another that net control cannot, and passes their traffic along. |
| Relay (the role) | A grantable session role — the lowest staff tier, permitted to log check-ins on this session. It is an **account**, and holding it says nothing about whether any particular check-in was relayed. See [Roles and permissions](roles-and-permissions.md). |
| Relayed by (the field) | A per-check-in record of **which station** passed that entry's traffic, kept as a callsign beside the entry. It is a **station**, not an account — the relaying station usually has no NetRoll account at all. A Logger sitting beside net control can record that W1ABC relayed a station, and an account holding the Relay **role** can log a check-in nobody relayed. Distinct from the entry's **way in**, which records how the traffic travelled rather than who passed it. Added 2026-09-02. |
| Roll call | A net type where net control works a list in order, as opposed to an open net where stations call in freely. |
| RS / RST | Signal report formats. RS for phone, RST for CW. |
| QSY | To change operating frequency. In NetRoll, retuning one of a live session's ways in — a net can be reachable several ways at once, and each carries its own frequency. |

## NetRoll concepts

| Term | Definition |
|------|-----------|
| Net definition | The reusable description of a net: title, description, band, mode, geography, repeater details, category, type. Versioned — editing increments the version. |
| Occurrence | One scheduled dated instance of a net, generated from its schedule. Appears in the upcoming list. |
| Session | One live run of a net. Snapshots the definition when it starts and renders from that snapshot for its whole life. |
| Snapshot (session) | The definition fields copied into a session at start. Why editing a net never disturbs a net already running. |
| Snapshot (transport) | The folded state plus latest `seq` sent once on a fresh WebSocket connect. |
| Discovery | The public landing surface: the active-now hero plus the filterable upcoming list. |
| Listed | The default visibility. The net appears in discovery. |
| Unlisted | The net never appears in discovery. Reachable only through its link token. |
| Link token | A non-guessable token in a net's permalink URL. Every net has one, whatever its visibility. Not derived from the net's id. |
| Favorite | A per-account marker putting a net in "My nets". |
| Roster | The ordered list of check-ins on a session. |
| Roster memory | Per-net memory of stations that have checked in before, used to prefill returning stations. |
| Working station | The station net control is working right now. At most one at a time, shown to everyone as the working cursor. |
| Round | One pass through the roster taking comments and traffic. Repeatable without losing prior notes. |
| Source badge | Shows how an entry got there: **Staff-entered** or **Self**. |
| Staying status | Whether a station is staying for comments or is in and out. |
| Soft lock | A short sliding lease on a check-in while an operator edits it. Others see the row read-only. |
| Your-turn indicator | The marker on a participant's own row when they're next up. |

## Real-time and technical

| Term | Definition |
|------|-----------|
| Event | One recorded fact in a session, named `noun.verb` in the past tense — `checkin.added`, `session.closed`. |
| Event log | The ordered, append-only record of a session. Serves as the live stream, the resume source, the audit trail, and the export source. |
| `seq` | The per-session monotonic sequence number on each event. Also the client's resume cursor. |
| Fold | Deterministic application of events in `seq` order to derive state. |
| Delta | A single event pushed to connected clients as it's appended. |
| Optimistic write | The originating client's own pending entry, shown before the server confirms it. Other viewers never see it. |
| Connection status | `live`, `catching up`, or `out of sync` — always shown as colour, icon, and label together. |
| Net paused | Not a connection problem. Net control's heartbeat stopped, so the roster is frozen and writes suspended. |
| Port | A trait declared in the pure domain crate. |
| Adapter | An implementation of a port — Postgres, SMTP, HTTP egress. |
| Egress client | The single SSRF-guarded outbound HTTP client all external calls go through. |
| KEK | The instance key-encryption key wrapping stored QRZ credentials. Injected at runtime, never in Postgres or backups. |
| Hero instance | The instance N1CCK operates at `netroll.n1cck.radio`. Operated on a best-effort basis with no availability guarantee — no SLA is offered. Self-hosters need none of its infrastructure. |

## Terms NetRoll deliberately avoids

| Avoided | Use instead | Why |
|---------|-------------|-----|
| "Net" for a live run | Session | A net is the recurring thing; a session is one run of it. |
| "Admin" for a net owner | Owner | Admin means platform admin, a different axis entirely. |
| "User" in prose | You, or operator | Documentation addresses the reader directly. |
| "Delete" for closing a net | Close, or archive | Closing appends an event; nothing is destroyed. |

## See also

- [Session lifecycle](../live-sessions/session-lifecycle.md)
- [Roles and permissions](roles-and-permissions.md)
- [Net visibility](../nets/net-visibility.md)
- [Architecture](https://github.com/nkbooth/netroll/blob/main/ARCHITECTURE.md) on GitHub
