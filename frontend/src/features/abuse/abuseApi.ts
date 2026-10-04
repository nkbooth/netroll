// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for the public abuse-report API. Unauthenticated —
 * the "Report abuse" affordance lives on public surfaces. Failures arrive as
 * RFC 9457 problem+json and surface as `ProblemError`. Reuses the shared
 * bot-mitigation fields (honeypot + form token), exactly like sign-in.
 */

import { throwProblem } from "../auth/authApi";
import type { BotMitigationFields } from "../botMitigation/botMitigation";

/** The report a user submits. Only `body` is required. */
export interface AbuseReportInput {
  /** The free-text report. */
  body: string;
  /** Optional contact (email/callsign) the reporter chooses to leave. */
  reporterContact?: string;
  /** Optional URL the reporter was viewing. */
  contextUrl?: string;
}

/**
 * Submits an abuse report. Resolves on the server's neutral 202 acknowledgement
 * (no id is returned — no confirmation oracle); a rate-limit or validation
 * failure rejects with a `ProblemError` carrying the slug.
 */
export async function submitAbuseReport(
  input: AbuseReportInput,
  bot?: BotMitigationFields,
): Promise<void> {
  const response = await fetch("/api/abuse-reports", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...input, ...bot }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}
