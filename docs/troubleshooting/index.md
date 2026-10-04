---
title: "Troubleshooting"
description: "Fixes for NetRoll problems organized by what you're seeing — sign-in failures, live sessions that stop updating, and self-hosted instances that won't boot."
docType: "landing"
lastVerified: "2026-09-14"
---

# Troubleshooting

Problems grouped by what you're looking at when they happen.

## Pages

- **[Signing in](signing-in.md)** — magic links that don't arrive, expire, or get rejected.
- **[Live sessions](live-sessions.md)** — rosters that stop updating, frozen nets, locked rows,
  and rejected edits.
- **[Self-hosting](self-hosting.md)** — instances that won't boot, mail that doesn't send, lost
  avatars, and features returning 503.

## Quick triage

| What you see | Start here |
|--------------|-----------|
| No magic-link email | [Signing in](signing-in.md) |
| "Reconnecting — catching up…" | [Live sessions](live-sessions.md) |
| "Lost the server — showing last known roster" | [Live sessions](live-sessions.md) |
| The net is paused and the roster is frozen | [Live sessions](live-sessions.md) |
| A row says someone else is editing | [Live sessions](live-sessions.md) |
| The container exits immediately on start | [Self-hosting](self-hosting.md) |
| A feature returns HTTP 503 | [Self-hosting](self-hosting.md) |
| Avatars disappeared after an upgrade | [Self-hosting](self-hosting.md) |

## Get help

File an issue at
[github.com/nkbooth/netroll/issues](https://github.com/nkbooth/netroll/issues).

Include the version you're running, whether you're on the hero instance or self-hosting, and —
for a boot failure — the exact error line. Configuration errors name the offending variable, so
that one line usually identifies the problem outright.

For abuse on an instance rather than a bug in the software, use the in-app **Report abuse**
affordance instead.
