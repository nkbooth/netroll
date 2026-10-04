// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { canViewerDo } from "./capabilities";
import type { ViewerCapability, ViewerRole } from "./capabilities";

/**
 * The client-side capability render-gate helper mirrors the
 * DOMAIN rank thresholds (`authz.rs`) exactly — it is a UX affordance gate only.
 * The server stays the sole authority. These tests assert the threshold
 * MATRIX (behavior/logic), never rendered text.
 */

const ALL_ROLES: ViewerRole[] = [
  "owner",
  "net-control",
  "logger",
  "relay",
  "participant",
];

const ALL_CAPS: ViewerCapability[] = [
  "view-console",
  "run-session",
  "log-check-in",
  "manage-roles",
  "edit-staff-fields",
  "edit-check-in",
  "reorder-roster",
  "set-worked-station",
  "annotate-session",
  "claim-control",
  "moderate",
  "export-session",
];

describe("canViewerDo", () => {
  it("grants view-console and log-check-in to relay and above only", () => {
    for (const cap of ["view-console", "log-check-in"] as const) {
      expect(canViewerDo("owner", cap)).toBe(true);
      expect(canViewerDo("net-control", cap)).toBe(true);
      expect(canViewerDo("logger", cap)).toBe(true);
      expect(canViewerDo("relay", cap)).toBe(true);
      expect(canViewerDo("participant", cap)).toBe(false);
    }
  });

  it("grants the logger-floor capabilities to logger and above only", () => {
    for (const cap of ["edit-staff-fields", "edit-check-in", "annotate-session"] as const) {
      expect(canViewerDo("owner", cap)).toBe(true);
      expect(canViewerDo("net-control", cap)).toBe(true);
      expect(canViewerDo("logger", cap)).toBe(true);
      expect(canViewerDo("relay", cap)).toBe(false);
      expect(canViewerDo("participant", cap)).toBe(false);
    }
  });

  it("grants the net-control-floor capabilities to net-control and owner only", () => {
    for (const cap of ["run-session", "manage-roles", "reorder-roster", "set-worked-station", "moderate"] as const) {
      expect(canViewerDo("owner", cap)).toBe(true);
      expect(canViewerDo("net-control", cap)).toBe(true);
      expect(canViewerDo("logger", cap)).toBe(false);
      expect(canViewerDo("relay", cap)).toBe(false);
      expect(canViewerDo("participant", cap)).toBe(false);
    }
  });

  it("grants moderate to net-control and owner only", () => {
    // Session moderation is an NCS act at the NetControl floor; a Logger's
    // detail-modal remove (edit-check-in) is a separate, lower-bar correction.
    expect(canViewerDo("owner", "moderate")).toBe(true);
    expect(canViewerDo("net-control", "moderate")).toBe(true);
    expect(canViewerDo("logger", "moderate")).toBe(false);
    expect(canViewerDo("relay", "moderate")).toBe(false);
    expect(canViewerDo("participant", "moderate")).toBe(false);
    expect(canViewerDo(null, "moderate")).toBe(false);
  });

  it("grants export-session to net-control and owner only", () => {
    // CSV/ADIF export is scoped to NCS/owners at the NetControl floor; a Logger
    // may edit check-ins but may never export the net-control-perspective log.
    expect(canViewerDo("owner", "export-session")).toBe(true);
    expect(canViewerDo("net-control", "export-session")).toBe(true);
    expect(canViewerDo("logger", "export-session")).toBe(false);
    expect(canViewerDo("relay", "export-session")).toBe(false);
    expect(canViewerDo("participant", "export-session")).toBe(false);
    expect(canViewerDo(null, "export-session")).toBe(false);
  });

  it("a relay holds exactly view-console and log-check-in — nothing more", () => {
    const held = ALL_CAPS.filter((cap) => canViewerDo("relay", cap));
    expect(held.sort()).toEqual(["log-check-in", "view-console"]);
  });

  it("owner holds every capability; participant holds none", () => {
    for (const cap of ALL_CAPS) {
      expect(canViewerDo("owner", cap)).toBe(true);
      expect(canViewerDo("participant", cap)).toBe(false);
    }
  });

  it("an absent/unknown viewer role (null) holds nothing — fail closed", () => {
    for (const cap of ALL_CAPS) {
      expect(canViewerDo(null, cap)).toBe(false);
    }
  });

  it("every role in the hierarchy holds exactly the capabilities at or below its rank", () => {
    // A stronger replacement for a prior vacuous "is a boolean" check (review
    // finding — that check could never fail even if a role's rank were
    // silently dropped, since a missing Record key evaluates `undefined >=
    // threshold` to `false`, which is still `typeof "boolean"`). This asserts
    // the FULL per-role capability SET, so any role whose rank regresses to
    // the wrong tier is caught by name.
    const expectedHeld: Record<ViewerRole, ViewerCapability[]> = {
      owner: [...ALL_CAPS],
      "net-control": [...ALL_CAPS],
      logger: ["view-console", "log-check-in", "edit-staff-fields", "edit-check-in", "annotate-session", "claim-control"],
      relay: ["view-console", "log-check-in"],
      participant: [],
    };
    for (const role of ALL_ROLES) {
      const held = ALL_CAPS.filter((cap) => canViewerDo(role, cap)).sort();
      expect(held).toEqual([...expectedHeld[role]].sort());
    }
  });
});
