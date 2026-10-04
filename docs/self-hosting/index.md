---
title: "Self-hosting"
description: "Run your own NetRoll instance: a container quickstart, optional analytics, upgrades, and the deployment model behind the single-image design."
docType: "landing"
audience: ["self-hoster"]
lastVerified: "2026-07-31"
---

# Self-hosting

NetRoll ships as one container image — a single binary serving the built SPA — plus a Postgres
database. Nothing in it requires infrastructure operated by N1CCK: no tailnet, no 1Password, no
particular host.

## Get started

- **[Self-hosting quickstart](quickstart.md)** — a working instance from a compose file in about
  ten minutes.

## Guides

- **[Use the admin dashboard](admin-dashboard.md)** — review abuse reports, disable an account,
  and read the audit log.
- **[Enable analytics](enable-analytics.md)** — optional, cookie-less, self-hosted Plausible.
  Off unless you turn it on.
- **[Upgrade an instance](upgrade-an-instance.md)** — pin a version, upgrade, and roll back.

## Learn more

- **[Deployment model](deployment-model.md)** — what's in the image, what state exists, and what
  the app does at boot.

## Reference

- **[Configuration](../reference/configuration.md)** — every environment variable, its default,
  and whether an invalid value is a hard boot error.
- **[Troubleshooting self-hosting](../troubleshooting/self-hosting.md)** — boot failures, mail
  problems, and lost avatars.

## What you're responsible for

| Concern | Handled by NetRoll | Handled by you |
|---------|--------------------|----------------|
| Schema migrations | Yes — applied at boot | — |
| TLS and the public hostname | No | Your reverse proxy |
| Outgoing email | Sends via SMTP | Supplying a working relay and a sender domain |
| Backups | No | Postgres dumps and the avatar volume |
| Exposure control | No | Your firewall and proxy |
