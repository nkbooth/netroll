// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Client half of the honeypot + timing bot mitigation.
 *
 * When a protected form (signup, net creation) renders, the client fetches a
 * signed, timestamped token from `GET /api/form-tokens` and attaches it — plus
 * an always-empty honeypot field — on submit. The server verifies both. When
 * mitigation is disabled the server returns a `null` token and the client sends
 * none, so the endpoints behave exactly as before. This module is I/O-thin and
 * fail-open: a token-fetch failure never blocks a legitimate submission.
 *
 * Fail-open asymmetry, deliberately kept: the SERVER
 * treats an absent token as unconditionally a bot (dropping the submission
 * with a success-shaped response, never surfacing an error). So a client-side
 * token-fetch failure on an instance with mitigation ENABLED silently loses a
 * genuine submission. This is judged an acceptable, narrow-edge tradeoff — the
 * whole point of the honeypot design is that ANY false positive degrades the
 * same, silent way a real bot drop does, and a resubmit typically self-heals a
 * transient failure — but the single retry below meaningfully narrows the
 * window without reintroducing server-side statefulness or PoW-level
 * complexity.
 */

/** Bot-mitigation fields attached to a protected form submission. */
export interface BotMitigationFields {
  /**
   * The honeypot value the form holds — empty for a human, filled by a bot.
   * Named `hpField` rather than something like `website`/`url`, which some
   * password managers/privacy extensions autofill into hidden inputs
   * regardless of visibility because the name matches a known profile field.
   */
  hpField: string;
  /** The signed form token; omitted when mitigation is disabled. */
  formToken?: string;
}

/** One attempt at `GET /api/form-tokens`. `ok: false` covers both a network
 * failure and a non-2xx response — either way the caller may retry. */
type FormTokenAttempt =
  | { ok: true; token: string | null }
  | { ok: false };

async function attemptFetchFormToken(): Promise<FormTokenAttempt> {
  try {
    const response = await fetch("/api/form-tokens", {
      credentials: "same-origin",
    });
    if (!response.ok) {
      return { ok: false };
    }
    const body = (await response.json()) as { formToken: string | null };
    return { ok: true, token: body.formToken ?? null };
  } catch {
    return { ok: false };
  }
}

/**
 * Fetches a form token, or `null` when mitigation is disabled on the instance
 * or the request could not be made after a retry. Fail-open by design.
 *
 * Retries ONCE on a genuine failure (network error, non-2xx) — a transient
 * blip, ad-blocker hiccup, or CDN error must not permanently masquerade as
 * "mitigation disabled" when a second attempt would have succeeded (see the
 * module-level fail-open note). Does NOT retry a well-formed `{ formToken:
 * null }` response — that is mitigation genuinely being off, not a failure.
 */
export async function fetchFormToken(): Promise<string | null> {
  const first = await attemptFetchFormToken();
  if (first.ok) {
    return first.token;
  }
  const second = await attemptFetchFormToken();
  return second.ok ? second.token : null;
}

/**
 * Merges the bot-mitigation fields into a request body: the honeypot value the
 * form currently holds, plus the token when one was issued. Pure — the single
 * place the wire shape is assembled, so both call sites and their tests agree.
 */
export function withBotMitigation<T extends object>(
  body: T,
  formToken: string | null,
  honeypot: string,
): T & BotMitigationFields {
  return {
    ...body,
    hpField: honeypot,
    ...(formToken !== null ? { formToken } : {}),
  };
}
