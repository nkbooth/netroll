// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Mode-shaped signal-report input affordance.
 *
 * The net's operating mode shapes ONLY the report input's label / aria-hint /
 * placeholder — never a backend format constraint (the server stores a bounded
 * string). That mode moved off `SessionMeta.definition` and onto the
 * session's connection set, so its source is now
 * `rfConnection(session.connections)?.mode` — the first way in that has a mode
 * at all — and an internet-only net has none, falling to the general shape. This is
 * the ham-radio domain crux: SSB/AM use RS (readability+strength), CW adds a
 * Tone digit (RST), digital modes report a signed dB SNR (WSJT-X), FM is
 * qualitative (full-quieting-style), and a mixed net has no fixed shape.
 */

/** The report format family a mode maps to (the mode-shaping LOGIC). */
export type ReportFormat = "rs" | "rst" | "db" | "qualitative" | "general";

/** The derived input affordance for the report field. */
export interface ReportShape {
  /** The format family — drives the placeholder/aria and is the testable logic. */
  readonly format: ReportFormat;
  /** The field label (mode-tagged, e.g. `Report · SSB`). */
  readonly label: string;
  /** The accessible hint assistive tech reads (e.g. `Signal report — RST`). */
  readonly ariaHint: string;
  /** An example value for the placeholder (empty for `mixed`, no fixed shape). */
  readonly placeholder: string;
}

/**
 * Maps an operating-mode token to its report input affordance. An unknown or
 * absent mode falls back to the general free-form shape (total, never throws).
 */
export function reportShapeForMode(mode: string | undefined): ReportShape {
  switch (mode) {
    case "ssb":
      return { format: "rs", label: "Report · SSB", ariaHint: "Signal report — RS", placeholder: "59" };
    case "am":
      return { format: "rs", label: "Report · AM", ariaHint: "Signal report — RS", placeholder: "59" };
    case "cw":
      return { format: "rst", label: "Report · CW", ariaHint: "Signal report — RST", placeholder: "599" };
    case "digital":
      return {
        format: "db",
        label: "Report · Digital",
        ariaHint: "Signal report — dB SNR",
        placeholder: "−06",
      };
    case "fm":
      return {
        format: "qualitative",
        label: "Report · FM",
        ariaHint: "Signal report — qualitative (optional)",
        placeholder: "full quieting",
      };
    default:
      // `mixed` and any unknown/absent token: per-contact mode varies, so no
      // fixed shape — general free-form.
      return { format: "general", label: "Report", ariaHint: "Signal report", placeholder: "" };
  }
}
