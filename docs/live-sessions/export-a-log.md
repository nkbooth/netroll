---
title: "Export a log"
description: "Download a closed NetRoll session as CSV for the full roster or ADIF for the net-control-perspective log, both generated from the session's event log."
docType: "howto"
audience: ["operator"]
lastVerified: "2026-09-04"
---

# Export a log

Get the log out after the net. CSV gives you the complete roster; ADIF gives you the net as
worked from the net-control perspective, for your logging program.

## Prerequisites

- A [closed session](start-and-close-a-session.md).
- The NCS or Owner role on it. Export is denied to participants with HTTP 403.

## Steps

### Step 1: Open the closed session

Open the session. Its summary renders from the folded event log.

### Step 2: Download CSV

Download the CSV export. It carries every captured field for every entry:

- Callsign, name, location and grid
- Role, source, entering operator
- Signal report, staying status
- Precedence and traffic
- Timestamps
- Per-station staff and public notes, and net-level notes
- **The way in** — which of the net's connections that station actually arrived on, written the
  way it reads on screen ("HF — 14.230 MHz", "EchoLink — 12345"), as that connection stood when
  the station checked in — a net that moved frequency afterwards does not rewrite it. The cell is
  empty when nobody recorded one, which is not the same as the station having arrived on the
  net's first connection. If net control typed a way in the net does not list, the cell carries
  their words.

### Step 3: Download ADIF

Download the ADIF export for a valid ADIF file representing the net as worked from the net
control station's perspective. Import it into your logging program.

**Each QSO is described by the way that station came in on, not by the net's headline
frequency.** On a cross-mode net that matters: a station that checked in over EchoLink gets
`PROP_MODE` `ECH` and **no band and no frequency**, because it had none — rather than being
stamped with the HF band the net also runs on. A station on AllStar, DMR, D-Star, System Fusion
or a URF reflector gets `PROP_MODE` `INTERNET` the same way. A station on a radio way in gets
`FREQ`, `BAND` and `MODE` from that connection. `FREQ` is the frequency that connection was on
when the station checked in: if the net moved frequency later in the session, the stations worked
before the move keep the frequency they were actually worked on.

Two cases fall outside that. A check-in with **no** recorded way in falls back to the connection
the export is about — the first one carrying a frequency, else the one the net leads with — so
an ordinary single-frequency net exports exactly as it always did. A check-in whose way in the
net does not list carries **no** band, mode, frequency or propagation tag at all; there is
nothing truthful to put there, and an ADIF file reaches LoTW and QRZ and cannot be recalled.

## Verify

Open the CSV. Its row count matches the roster in the on-screen summary, and the entries are in
the same order — both are generated from the same event log, so the export reflects the session
exactly as it ran.

## Spreadsheet safety

Any field that starts with a spreadsheet formula character is escaped when the CSV is generated,
so opening the file in a spreadsheet can't execute a value someone typed into a callsign or a
note.

## Getting the log delivered automatically

Rather than downloading by hand every week, configure per-net email addresses and a webhook.
NetRoll delivers the summary to them the moment the session closes — including when the session
was auto-closed after being abandoned. See [configure delivery](../nets/configure-delivery.md).

## Related guides

- [Start and close a session](start-and-close-a-session.md)
- [Configure delivery](../nets/configure-delivery.md)
- [Export or delete your data](../account/export-or-delete-your-data.md) — your own personal
  data, as opposed to a net's log
