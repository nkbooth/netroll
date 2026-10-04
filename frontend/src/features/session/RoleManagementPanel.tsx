// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { canViewerDo } from "./capabilities";
import type { ViewerRole } from "./capabilities";
import { grantRole, listRoles, revokeRole } from "./sessionApi";
import type { GrantableRole, RoleGrant } from "./sessionApi";
import { tokens } from "../../ui/tokens/tokens";

/**
 * The role-management control INSIDE the staff console (EXPERIENCE.md:41 — "role management lives inside the staff console as
 * controls, not separate surfaces"). It lists the current explicit grants by
 * callsign, grants the Relay role (or Logger) to a station by callsign, and
 * revokes a grant. It consumes the already-shipped grant and revoke endpoints
 * and the roles-list read.
 *
 * Rendered ONLY for a viewer holding `ManageRoles` (NCS/Owner) — it self-gates
 * on `viewerRole` so a Logger/Relay/Participant never sees it. This is a UX
 * affordance gate: the SERVER remains the sole authority — every
 * grant/revoke is re-checked server-side and a tampered client still gets a 403.
 *
 * The picker offers only the roles the actor may grant (Relay, Logger) — never
 * `net-control`/`owner` (the server's `can_manage_role` ceiling refuses them
 * anyway; owner-set changes go through the net-definition owner endpoints).
 * Grants emit NO session event — they are a plain table — so the panel refetches
 * its own list after every grant/revoke rather than awaiting a WS delta.
 */

const sectionStyle: CSSProperties = {
  margin: "var(--space-4) 0",
  padding: "var(--space-3) var(--space-4)",
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
};

const headingStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  color: "var(--text-muted)",
  textTransform: "uppercase",
  letterSpacing: "0.04em",
  margin: "0 0 var(--space-2)",
};

const listStyle: CSSProperties = { listStyle: "none", padding: 0, margin: 0 };

const rowStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  alignItems: "center",
  marginBottom: "var(--space-1)",
};

const controlStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-2)",
  background: "var(--surface)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
};

const primaryButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const secondaryButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-3)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const formStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  marginTop: "var(--space-2)",
  flexWrap: "wrap",
};

const errorStyle: CSSProperties = { color: "var(--sync-text)", margin: "var(--space-2) 0 0" };

/** The grantable roles offered in the picker — never net-control/owner. */
const GRANTABLE_ROLES: readonly GrantableRole[] = ["relay", "logger"];

export interface RoleManagementPanelProps {
  readonly sessionId: string;
  /**
   * The viewer's own resolved role. The panel renders ONLY when this holds
   * `ManageRoles` (NCS/Owner); it returns `null` otherwise.
   */
  readonly viewerRole: ViewerRole | null;
}

/** The in-console grant/list/revoke role-management panel. */
export function RoleManagementPanel({
  sessionId,
  viewerRole,
}: RoleManagementPanelProps): ReactElement | null {
  const [grants, setGrants] = useState<RoleGrant[]>([]);
  const [addCallsign, setAddCallsign] = useState("");
  const [addRole, setAddRole] = useState<GrantableRole>("relay");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);

  const canManage = canViewerDo(viewerRole, "manage-roles");

  const reloadGrants = useCallback(async (): Promise<void> => {
    const current = await listRoles(sessionId);
    setGrants(current);
  }, [sessionId]);

  // Grants emit no WS event — the panel loads its own list, and
  // refetches after each mutation. Only load once the viewer may manage roles.
  useEffect(() => {
    if (!canManage) {
      return;
    }
    // A failed initial load surfaces as the mapped error, never an unhandled
    // rejection (the same non-throwing posture the owner panel uses).
    void reloadGrants().catch((error: unknown) => {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
    });
  }, [canManage, reloadGrants]);

  // Grant/revoke reload the list REGARDLESS of the mutation's outcome (review
  // finding): a missing-grant 404 (e.g. revoking a row the server already
  // dropped) means the panel's list is stale relative to server truth, and
  // only a refetch reconciles the two — without it, the now-nonexistent row's
  // Revoke control would stay clickable forever, always re-404ing. A refusal
  // (over-ceiling 403, unknown-callsign 404, missing-grant 404) surfaces via
  // the mapped alert; the mutation's own error takes priority over a
  // subsequent reload failure, since it is the more actionable one to show.
  // Mirrors the owner panel's runOwnerOp shape.
  const runRoleOp = async (op: () => Promise<unknown>): Promise<void> => {
    setProblem(null);
    setBusy(true);
    let opError: unknown;
    try {
      await op();
    } catch (error: unknown) {
      opError = error;
    }
    try {
      await reloadGrants();
    } catch (reloadError: unknown) {
      if (opError === undefined) {
        opError = reloadError;
      }
    }
    setProblem(
      opError === undefined ? null : opError instanceof ProblemError ? opError.problem : undefined,
    );
    setBusy(false);
  };

  if (!canManage) {
    return null;
  }

  const handleGrant = (): void => {
    const callsign = addCallsign.trim();
    if (callsign === "") {
      return;
    }
    void runRoleOp(async () => {
      await grantRole(sessionId, { callsign, role: addRole });
      setAddCallsign("");
    });
  };

  const handleRevoke = (accountId: string): void => {
    void runRoleOp(() => revokeRole(sessionId, accountId));
  };

  return (
    <section style={sectionStyle} aria-label="Role management">
      <h2 style={headingStyle}>Roles</h2>
      <ul style={listStyle}>
        {grants.map((grant) => {
          const label = grant.callsign ?? grant.accountId;
          return (
            <li key={grant.accountId} style={rowStyle}>
              <span>{label}</span>
              <span style={{ color: "var(--text-muted)" }}>{grant.role}</span>
              <button
                type="button"
                aria-label={`Revoke ${label}`}
                onClick={() => handleRevoke(grant.accountId)}
                disabled={busy}
                style={secondaryButtonStyle}
              >
                Revoke
              </button>
            </li>
          );
        })}
      </ul>
      <div style={formStyle}>
        <input
          type="text"
          aria-label="Grant role to station"
          autoCapitalize="characters"
          autoComplete="off"
          value={addCallsign}
          onChange={(event) => setAddCallsign(event.target.value)}
          disabled={busy}
          style={controlStyle}
        />
        <select
          aria-label="Role to grant"
          value={addRole}
          onChange={(event) => setAddRole(event.target.value as GrantableRole)}
          disabled={busy}
          style={controlStyle}
        >
          {GRANTABLE_ROLES.map((role) => (
            <option key={role} value={role}>
              {role}
            </option>
          ))}
        </select>
        <button
          type="button"
          onClick={handleGrant}
          disabled={busy}
          style={primaryButtonStyle}
        >
          Grant role
        </button>
      </div>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </section>
  );
}
