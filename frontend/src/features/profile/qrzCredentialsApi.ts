// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for the write-only QRZ credential surface. Cookie
 * authenticated like the other account endpoints; failures arrive as
 * problem+json. The stored callsign/password are NEVER fetched or returned —
 * only `qrzCredentialsSet` on the account body reveals whether they exist.
 */

import { throwProblem } from "../auth/authApi";

/**
 * Stores (or replaces) the signed-in account's QRZ callbook credentials. The
 * server seals them under envelope encryption; nothing is returned on success.
 * A 503 `/errors/crypto-unavailable` (no KEK on this instance) or a 422
 * `/errors/qrz-credentials-invalid` surfaces as a `ProblemError`.
 */
export async function setQrzCredentials(
  callsign: string,
  password: string,
): Promise<void> {
  const response = await fetch("/api/accounts/me/qrz-credentials", {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ callsign, password }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/**
 * Clears any stored QRZ credentials for the signed-in account. Idempotent on
 * the server; non-OK responses throw `ProblemError`.
 */
export async function clearQrzCredentials(): Promise<void> {
  const response = await fetch("/api/accounts/me/qrz-credentials", {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}
