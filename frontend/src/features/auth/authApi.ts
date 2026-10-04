// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for the auth API. Same-origin cookies carry the session;
 * failures arrive as RFC 9457 problem+json and surface as `ProblemError`.
 */

import type { BotMitigationFields } from "../botMitigation/botMitigation";

/** Wire shape of an RFC 9457 problem response. */
export interface Problem {
  type: string;
  title?: string;
  status: number;
  detail?: string;
  /** RFC 9457 extension member: which entry of a submitted connection list was
   * refused, zero-based, as this client numbered it. Absent on
   * every problem that is not about one connection — including a refusal about
   * the connection SET, which belongs at the list head rather than on a row. */
  connectionIndex?: number;
}

/** The signed-in account as returned by the API (camelCase wire). */
export interface Account {
  id: string;
  email: string;
  emailVerifiedAt: string | null;
  /** Whether the consent gate must be shown before gated actions. */
  consentRequired: boolean;
  /** The terms version the server currently requires — never hardcoded. */
  requiredTermsVersion: string;
  /** The operator's reserved base callsign, or `null` before one is set. */
  callsign: string | null;
  /** Chosen display name, or `null` before one is set. */
  displayName: string | null;
  /** Free-text location ("Hartford, CT"), or `null` before one is set. */
  location: string | null;
  /** Canonical Maidenhead grid locator (`FN31pr`), or `null` before set. */
  grid: string | null;
  /** User-supplied HTTPS avatar URL, or `null` before one is set. */
  avatarUrl: string | null;
  /** Always present — derived server-side from the account email. */
  gravatarUrl: string;
  /**
   * Whether QRZ callbook credentials are stored. Write-only: the
   * stored callsign/password are never returned — only this boolean.
   */
  qrzCredentialsSet: boolean;
  /**
   * Whether this account is a platform admin (the backend `AccountBody`'s
   * `isAdmin`, from the boot `ADMIN_ACCOUNT_EMAILS` allowlist). Drives whether
   * the shell offers the admin entry at all; the server gate is the authority.
   */
  isAdmin: boolean;
}

/** A failed API call, carrying the parsed problem for slug-based mapping. */
export class ProblemError extends Error {
  readonly problem: Problem;

  constructor(problem: Problem) {
    super(problem.title ?? problem.type);
    this.name = "ProblemError";
    this.problem = problem;
  }
}

const JSON_HEADERS = { "content-type": "application/json" };

/** Parses a failed response's problem+json and throws it as `ProblemError`. */
export async function throwProblem(response: Response): Promise<never> {
  let problem: Problem;
  try {
    problem = (await response.json()) as Problem;
  } catch {
    problem = { type: "/errors/unknown", status: response.status };
  }
  throw new ProblemError(problem);
}

/** Requests a magic link for `email`. Resolves on 202. */
export async function requestMagicLink(
  email: string,
  bot?: BotMitigationFields,
): Promise<void> {
  const response = await fetch("/api/magic-links", {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ email, ...bot }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/** Consumes a magic-link token, establishing the cookie session. */
export async function createSession(token: string): Promise<Account> {
  const response = await fetch("/api/sessions", {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ token }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as Account;
}

/** The current account, or null when signed out (401 is not an error). */
export async function fetchCurrentAccount(): Promise<Account | null> {
  const response = await fetch("/api/accounts/me", {
    credentials: "same-origin",
  });
  if (response.status === 401) {
    return null;
  }
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as Account;
}

/** Revokes the current session server-side (sign-out). */
export async function deleteCurrentSession(): Promise<void> {
  const response = await fetch("/api/sessions/current", {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok && response.status !== 401) {
    await throwProblem(response);
  }
}
