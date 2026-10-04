// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrappers for the bounded admin API and the admin dashboard. Every
 * call is session-cookied and additionally admin-gated
 * server-side: a non-admin gets `/errors/forbidden` from all of them.
 *
 * Wire types mirror the Rust views in `backend/.../http/admin.rs` one-for-one.
 * Failures arrive as RFC 9457 problem+json and surface as `ProblemError`.
 */

import { throwProblem } from "../auth/authApi";

/** One unresolved abuse report (the Rust `AbuseReportView`). */
export interface AbuseReport {
  id: string;
  createdAt: string;
  /** Contact the reporter chose to leave, or `null`. */
  reporterContact: string | null;
  /** The free-text report. */
  body: string;
  /** The page the reporter was on when they filed, or `null`. */
  contextUrl: string | null;
}

/** One security audit record (the Rust `AuditLogView`). */
export interface AuditEntry {
  id: string;
  occurredAt: string;
  actorAccountId: string;
  /** The closed kebab action vocabulary (`disable-account`, `signed-in`, …). */
  action: string;
  targetType: string | null;
  targetId: string | null;
  /** Bounded non-PII detail (booleans, roles, ids), or `null`. */
  metadata: unknown;
}

/** The object kinds the admin search can return. */
export type AdminObjectType =
  | "account"
  | "net-definition"
  | "net-session"
  | "abuse-report";

/** One object matched by the admin search (the Rust `SearchHitView`). */
export interface AdminSearchHit {
  objectType: AdminObjectType;
  /** The id every action endpoint and audit filter takes. */
  id: string;
  /** Callsign or net title; `null` when the object has no human name. */
  label: string | null;
  /** Email, session lifecycle, or `null`. */
  sublabel: string | null;
  /** When an admin disabled it; accounts only. Picks Disable vs Re-enable. */
  disabledAt: string | null;
  /** When it was archived, closed, resolved, or self-deleted. */
  inactiveAt: string | null;
}

/** What one admin search answered: the matches, and which types were cut. */
export interface AdminSearchResult {
  hits: AdminSearchHit[];
  /**
   * Object types for which more matched than the server's per-type cap served
   * (a type matching exactly the cap is complete). Empty when every searched
   * type is complete; a type that was not searched never appears.
   */
  truncatedTypes: AdminObjectType[];
}

/** The audit log's investigation filters. */
export interface AuditFilters {
  /** "What did this account do?" */
  actor?: string;
  /** "What touched this object?" — account, report, session, or net. */
  object?: string;
  /** A single action verb from the closed vocabulary. */
  action?: string;
}

/**
 * One page of a keyset-paginated admin read. `nextCursor` is `null` on the last
 * page — the caller offers "load more" only while it is a string.
 */
export interface Page<T> {
  items: T[];
  nextCursor: string | null;
}

/** Builds an admin URL, omitting params the caller left unset. */
function adminUrl(path: string, params: Record<string, string | undefined>) {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) {
      search.set(key, value);
    }
  }
  const query = search.toString();
  return query ? `${path}?${query}` : path;
}

/** GETs an admin endpoint, throwing the problem slug on any failure. */
async function readAdmin<T>(url: string): Promise<T> {
  const response = await fetch(url, { credentials: "same-origin" });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as T;
}

/** POSTs a body-less admin action, throwing the problem slug on any failure. */
async function postAdmin(url: string): Promise<void> {
  const response = await fetch(url, {
    method: "POST",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/**
 * One page of the unresolved abuse-report queue, oldest first.
 *
 * `cursor` must be a `nextCursor` this server previously returned — it is
 * opaque, and the server refuses anything it did not issue rather than
 * restarting the list.
 */
export function fetchAbuseReports(cursor?: string): Promise<Page<AbuseReport>> {
  return readAdmin(adminUrl("/api/admin/abuse-reports", { cursor }));
}

/** Marks a report resolved, clearing it from the queue. Idempotent server-side. */
export function resolveAbuseReport(id: string): Promise<void> {
  return postAdmin(
    `/api/admin/abuse-reports/${encodeURIComponent(id)}/resolve`,
  );
}

/**
 * One page of the security audit log, newest first. Reading it is itself audited.
 *
 * `filters` narrows to one actor, one object, or one action verb. The returned
 * `nextCursor` is bound to the filter set that produced it — replaying it under
 * different filters is refused rather than silently re-anchoring, so always pass
 * the same `filters` alongside a cursor.
 */
export function fetchAuditLog(
  filters: AuditFilters = {},
  cursor?: string,
): Promise<Page<AuditEntry>> {
  return readAdmin(
    adminUrl("/api/admin/audit-log", {
      cursor,
      actor: filters.actor,
      object: filters.object,
      action: filters.action,
    }),
  );
}

/**
 * Resolves business objects by name or id — the way to obtain the UUID every
 * other admin surface requires.
 *
 * Callsigns and display names match by PREFIX and net/session titles by
 * substring; **email and every id match exactly**, so this cannot be used to
 * enumerate addresses. A bare uuid resolves back to whichever object it names,
 * which is how a value copied out of the audit log becomes readable.
 *
 * Results are capped per object type; `truncatedTypes` names the types for
 * which more matched than were served, so a cut result can be told from a
 * complete one. A 200 without that field is a contract break and rejects.
 *
 * A blank or over-long term is refused server-side with `/errors/validation`.
 */
export async function searchObjects(
  q: string,
  type?: AdminObjectType,
): Promise<AdminSearchResult> {
  const body = await readAdmin<{
    items: AdminSearchHit[];
    truncatedTypes: AdminObjectType[];
  }>(adminUrl("/api/admin/search", { q, type }));
  // Our own server always sends the signal, so its absence is a contract
  // break. It surfaces here, as the tab's error state, rather than as a
  // silent "complete" (`?? []`, ruled out) or as a TypeError inside render.
  if (!Array.isArray(body.truncatedTypes)) {
    throw new Error("admin search response carries no truncatedTypes");
  }
  return { hits: body.items, truncatedTypes: body.truncatedTypes };
}

/**
 * Disables an account instance-wide: it can no longer act or sign back in, and
 * its live sessions are revoked. Reversible only via {@link reenableAccount} —
 * signing in never clears it. Disabling your own account is refused
 * (`/errors/cannot-disable-self`).
 */
export function disableAccount(id: string): Promise<void> {
  return postAdmin(`/api/admin/accounts/${encodeURIComponent(id)}/disable`);
}

/** Clears an account's disabled state — the only path that does. */
export function reenableAccount(id: string): Promise<void> {
  return postAdmin(`/api/admin/accounts/${encodeURIComponent(id)}/reenable`);
}
