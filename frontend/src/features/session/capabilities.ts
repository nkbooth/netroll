// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Client-side capability render-gate — the TypeScript mirror
 * of the DOMAIN authorization thresholds in `netroll-domain/src/authz.rs`. It
 * encodes the SAME `Owner ⊃ NetControl ⊃ Logger ⊃ Relay ⊃ Participant`
 * containment chain as one rank comparison, so the console can hide affordances
 * a viewer's role could never exercise (a granted relay is add-only).
 *
 * This is a UX affordance gate ONLY: every mutation
 * stays server-enforced, and a tampered client that renders a control it should
 * not still gets a 403. It never REPLACES a server check — it only avoids
 * showing a control that would only fail. The thresholds are encoded ONCE here
 * (not scattered `if role === …`), mirroring the domain's single-comparison
 * model.
 */

/** The lowercase-kebab viewer-role wire tokens (mirrors the domain `Role`). */
export type ViewerRole =
  | "owner"
  | "net-control"
  | "logger"
  | "relay"
  | "participant";

/**
 * The server-enforced capabilities this gate reasons about (mirrors the domain
 * `Capability`). Kebab tokens; the set is additive-only, like the domain enum.
 */
export type ViewerCapability =
  | "view-console"
  | "run-session"
  | "log-check-in"
  | "manage-roles"
  | "edit-staff-fields"
  | "edit-check-in"
  | "reorder-roster"
  | "set-worked-station"
  | "set-roster-order-mode"
  | "annotate-session"
  | "claim-control"
  | "moderate"
  | "export-session";

/** The numeric rank of a role: `owner` = 4 down to `participant` = 0. */
const ROLE_RANK: Record<ViewerRole, number> = {
  owner: 4,
  "net-control": 3,
  logger: 2,
  relay: 1,
  participant: 0,
};

/**
 * The minimum role rank that holds each capability — the domain `min_rank`
 * thresholds: view/log at Relay (1), edit/annotate at Logger (2), run/manage/
 * reorder/worked at NetControl (3).
 */
const CAPABILITY_MIN_RANK: Record<ViewerCapability, number> = {
  "view-console": 1,
  "log-check-in": 1,
  "edit-staff-fields": 2,
  "edit-check-in": 2,
  "annotate-session": 2,
  // The involuntary-claim rescue capability at the Logger floor
  // (deliberately LOWER than run-session's NetControl floor). Necessary but not
  // sufficient — the endpoint also requires the session to be stalled.
  "claim-control": 2,
  "run-session": 3,
  "manage-roles": 3,
  "reorder-roster": 3,
  "set-worked-station": 3,
  // The SHARED roster ordering mode changes what every viewer sees,
  // so it sits at the NetControl floor alongside reorder-roster.
  "set-roster-order-mode": 3,
  // NCS disciplinary remove/block at the NetControl floor —
  // a materially higher bar than edit-check-in's Logger-floor correction.
  moderate: 3,
  // CSV/ADIF export is scoped to NCS/owners at the NetControl
  // floor. The UI additionally only surfaces the link post-close; the server
  // enforces the capability regardless of lifecycle.
  "export-session": 3,
};

/**
 * Whether a viewer holding `role` may exercise `capability` — true iff the
 * role's rank meets the capability's threshold. A `null` role (an absent
 * `viewerRole`, e.g. the account-less public view) fails closed: it holds
 * nothing. UX gate only — the server remains authoritative.
 */
export function canViewerDo(
  role: ViewerRole | null,
  capability: ViewerCapability,
): boolean {
  if (role === null) {
    return false;
  }
  return ROLE_RANK[role] >= CAPABILITY_MIN_RANK[capability];
}
