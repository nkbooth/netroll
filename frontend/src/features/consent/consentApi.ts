// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for the consent API. Cookie-authenticated like the auth
 * endpoints; failures arrive as problem+json.
 */

import { throwProblem } from "../auth/authApi";

/**
 * Records consent to `termsVersion` for the signed-in account. Resolves on
 * 201 (idempotent server-side); a stale version rejects with
 * `/errors/consent-version-mismatch`.
 */
export async function recordConsent(termsVersion: string): Promise<void> {
  const response = await fetch("/api/consents", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ termsVersion }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}
