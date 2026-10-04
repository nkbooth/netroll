---
title: "Reference"
description: "Authoritative reference for NetRoll: environment variables, the HTTP API, the WebSocket protocol, the role capability matrix, and the glossary."
docType: "landing"
lastVerified: "2026-09-14"
---

# Reference

Structured facts about NetRoll. These pages answer "what exactly does X do" and "what are the
valid values for Y" — for how-to guidance, start from the section that matches your task.

## Pages

- **[Configuration](configuration.md)** — every environment variable, its default, and whether an
  invalid value stops the app booting.
- **[HTTP API](http-api.md)** — every REST endpoint with its method, authorization, and error
  semantics.
- **[WebSocket protocol](websocket-protocol.md)** — snapshot and delta frames, the `since`
  resume cursor, event kinds, and public-stream redaction.
- **[Webhook payload](webhook-payload.md)** — the on-close webhook request, its HMAC signature,
  and every payload field, with a worked example.
- **[Discord message](discord-message.md)** — the on-close Discord announcement: the embed shape,
  its size caps, and why it carries no signature and no roster.
- **[Roles and permissions](roles-and-permissions.md)** — the capability matrix across owners,
  net control, loggers, relays, participants, and viewers.
- **[Glossary](glossary.md)** — every term NetRoll uses, including the ones with a
  NetRoll-specific meaning.
- **[Vocabulary](vocabulary.md)** — the one word used for each concept, in code and in prose,
  and the synonyms that are rejected.

## Where to go instead

| If you want to | Go to |
|----------------|-------|
| Stand up an instance | [Self-hosting quickstart](../self-hosting/quickstart.md) |
| Understand how the code is shaped | [ARCHITECTURE.md](https://github.com/nkbooth/netroll/blob/main/ARCHITECTURE.md) on GitHub |
| Run a net | [Live sessions](../live-sessions/index.md) |
| Fix something that's broken | [Troubleshooting](../troubleshooting/index.md) |
