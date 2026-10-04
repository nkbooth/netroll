// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Public instance settings (`GET /api/app-config`): which optional
 * integrations this operator opted into.
 *
 * Read at RUNTIME rather than baked in as `VITE_*` constants because the
 * published container image is built once and run by self-hosters who cannot
 * rebuild the SPA — build-time values would make these unreachable for anyone
 * but the hero instance.
 */

/** The instance settings, with `null` meaning "this integration is off". */
export interface AppConfig {
  readonly plausibleDomain: string | null;
  /** Origin of a self-hosted Plausible, which the tracker posts events to;
   * `null` means Plausible's own host. */
  readonly plausibleScriptHost: string | null;
  readonly kofiUsername: string | null;
}

/** Everything off — the default posture, and the fallback when the read fails. */
const ALL_OFF: AppConfig = {
  plausibleDomain: null,
  plausibleScriptHost: null,
  kofiUsername: null,
};

/**
 * Fetches the instance settings, resolving to [`ALL_OFF`] on any failure.
 *
 * Deliberately never throws: analytics and a donation link are decoration, and
 * a failed config read must not break the page that was about to render.
 */
export async function fetchAppConfig(): Promise<AppConfig> {
  try {
    const response = await fetch("/api/app-config");
    if (!response.ok) {
      return ALL_OFF;
    }
    const body = (await response.json()) as Partial<AppConfig>;
    return {
      plausibleDomain: body.plausibleDomain ?? null,
      plausibleScriptHost: body.plausibleScriptHost ?? null,
      kofiUsername: body.kofiUsername ?? null,
    };
  } catch {
    return ALL_OFF;
  }
}

/** Plausible's own origin, used when the operator self-hosts nothing. */
const PLAUSIBLE_DEFAULT_HOST = "https://plausible.io";

/** Builds the Plausible event API URL for the configured (or default) host. */
export function plausibleEndpoint(scriptHost: string | null): string {
  const host = (scriptHost ?? PLAUSIBLE_DEFAULT_HOST).replace(/\/+$/, "");
  return `${host}/api/event`;
}
