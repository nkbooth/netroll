// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * The net taxonomy token lists, mirroring the domain enum vocabularies
 * (`net/enums.rs`) — the single frontend source of truth for the band / mode /
 * category / type / tone / visibility dropdowns and the discovery filter bar.
 * The server is authoritative; these only drive the selects (do NOT duplicate
 * them per-feature).
 */

export const BANDS = [
  "2200m", "630m", "160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m",
  "12m", "10m", "6m", "4m", "2m", "1.25m", "70cm", "33cm", "23cm", "other",
];
export const MODES = ["ssb", "cw", "am", "fm", "digital", "mixed"];
export const CATEGORIES = [
  "traffic", "emergency", "ares-races", "dx", "contest", "rag-chew",
  "technical", "swap", "club", "training", "other",
];
export const NET_TYPES = ["open", "roll-call"];
export const TONE_MODES = ["ctcss", "dcs", "split"];
/**
 * The DMR networks an operator is most likely to name — SUGGESTIONS, never a
 * permitted set.
 *
 * Unlike every other list in this file, this one mirrors no domain enum: a DMR
 * network name is free text, because networks appear and merge and
 * a private Hytera XPT system has a name nobody will ever enumerate. Editing
 * this list needs no migration, which is the point — do not turn it into a
 * `<select>` and do not validate against it.
 */
export const DMR_NETWORKS = ["Brandmeister", "TGIF", "FreeDMR"];
export const VISIBILITIES = ["listed", "unlisted"];
