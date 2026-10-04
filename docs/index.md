---
title: "NetRoll documentation"
description: "Documentation for NetRoll by N1CCK — run amateur-radio nets in the browser, join a net as a participant, self-host an instance, or contribute to the code."
docType: "landing"
lastVerified: "2026-09-14"
---

# NetRoll documentation

NetRoll by N1CCK is a browser-first app for running amateur-radio **nets** — coordinated
on-air meetups where stations check in, exchange signal reports, and follow a
net-control-moderated agenda. This documentation covers running a net, joining one,
self-hosting an instance, and working on the code.

## Start here

**[Create an account](getting-started/create-an-account.md)** — three minutes, and everything
else follows from it. Then run your first net, or join someone else's.

Just want to watch a net? That needs no account at all — go straight to
[join a net](getting-started/join-a-net.md).

## Or jump to your track

- **[Run nets](getting-started/run-your-first-net.md)** — you're a net owner or net control
  station (NCS) and want a live session with stations checking in.
- **[Self-host NetRoll](self-hosting/quickstart.md)** — you want to run your own instance.
  Nothing in NetRoll depends on infrastructure operated by N1CCK.
- **Work on the code?** Read [CONTRIBUTING.md](https://github.com/nkbooth/netroll/blob/main/CONTRIBUTING.md) on GitHub; the code's shape is in
  [ARCHITECTURE.md](https://github.com/nkbooth/netroll/blob/main/ARCHITECTURE.md).

## Sections

These follow the order you'd meet them: learn it, set a net up, run it, then the operator
track, with lookup material last.

| Section | What's in it |
|---------|--------------|
| [Getting started](getting-started/index.md) | Create an account, then run or join your first net |
| [Nets](nets/index.md) | Create, schedule, publish, and co-own a net definition |
| [Live sessions](live-sessions/index.md) | Run a net: check-ins, roster, roles, handoff, export |
| [Your account](account/index.md) | Sign-in, callsign, QRZ credentials, data rights |
| [Troubleshooting](troubleshooting/index.md) | Fixes organized by what you're seeing |
| [Self-hosting](self-hosting/index.md) | Deploy and operate your own instance |
| [Reference](reference/index.md) | Configuration, HTTP API, WebSocket protocol, roles, glossary, vocabulary |

## New to the terminology?

NetRoll uses amateur-radio net vocabulary throughout — *net*, *NCS*, *check-in*, *roll call*,
*precedence*, *relay*. The [glossary](reference/glossary.md) defines every term the app uses,
including the ones that mean something specific inside NetRoll (a *net definition* is not the
same thing as a *net session*).
