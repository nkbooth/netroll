# Changelog

## [1.0.2](https://github.com/nkbooth/netroll/compare/v1.0.1...v1.0.2) (2026-10-05)


### Documentation

* name the components bundled inside the vendored mermaid file ([#10](https://github.com/nkbooth/netroll/issues/10)) ([070340a](https://github.com/nkbooth/netroll/commit/070340a99e28768b36302eab79dc25e7c8e7ab0e))

## [1.0.1](https://github.com/nkbooth/netroll/compare/v1.0.0...v1.0.1) (2026-10-04)


### Bug Fixes

* harden the deploy dispatch, settings writes and licence check ([f04e221](https://github.com/nkbooth/netroll/commit/f04e221f65680f5debd09496eb3247a1a35e27b4))

## [1.0.0]

NetRoll was developed in a private repository through version 1.11.0; this is its first public release.

- A public directory of live and upcoming nets, filterable by name, band, mode, connection
  kind and country.
- Net definitions with one or more ways in: HF, repeater, EchoLink, AllStar, DMR, D-STAR, YSF,
  URF or another you name.
- Schedules with recurrence that follows the net's own time zone through daylight-saving
  changes.
- Live sessions with a shared roster, a working station and rounds, updated in every open
  browser as they happen.
- Self check-in from a participant's own browser, marked as such on the roster.
- Per-net roles: owner, net control, logger and relay, each with its own permissions.
- Hand-off of net control to another operator, and a claim path when net control drops out.
- Moderation of a session: check-ins can be hidden or removed by the people running it.
- CSV and ADIF export of the roster when a net closes.
- On-close delivery by email, signed webhook or Discord.
- QRZ callbook lookups, with credentials stored encrypted under an instance key.
- Passwordless sign-in by magic link.
- A small admin surface for abuse reports and account disabling.
- One container image plus PostgreSQL, self-hostable, serving its own documentation at `/docs`.
