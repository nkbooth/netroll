// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useState } from "react";

import { fetchAppConfig, plausibleEndpoint } from "./appConfigApi";
import type { AppConfig } from "./appConfigApi";

/**
 * Reads the instance settings once per page load and, when a Plausible domain
 * is configured, starts the analytics tracker.
 *
 * Nothing third-party loads on a default instance: no domain, no tracker chunk,
 * no request to any external host. The Ko-fi integration is a plain link, so it
 * needs no script at all — the hook just reports the username.
 */

const ALL_OFF: AppConfig = {
  plausibleDomain: null,
  plausibleScriptHost: null,
  kofiUsername: null,
};

/** The in-flight/settled config, shared process-wide so N consumers make ONE
 * request and the script is injected once regardless of how many components
 * read it. */
let cached: Promise<AppConfig> | undefined;

const loadOnce = (): Promise<AppConfig> => {
  cached ??= fetchAppConfig();
  return cached;
};

/** Set before the import is awaited, so N concurrent consumers start the
 * tracker once — `init` is documented as callable once, and a second call
 * would double-count every pageview. */
let trackerStarted = false;

/** Starts the Plausible tracker, or does nothing if it is already running. */
async function ensurePlausible(config: AppConfig): Promise<void> {
  if (config.plausibleDomain === null || trackerStarted) {
    return;
  }
  trackerStarted = true;
  try {
    // Dynamic so the tracker lands in its own chunk: an instance with
    // analytics off never fetches it, and it stays off the critical path.
    // Deep path because the package declares only `module`/`types` (no `main`,
    // no `exports`), which the bare specifier cannot be resolved from.
    const { init } = await import("@plausible-analytics/tracker/plausible.js");
    init({
      domain: config.plausibleDomain,
      endpoint: plausibleEndpoint(config.plausibleScriptHost),
    });
  } catch (error) {
    // Analytics is decoration, so this must not reach the caller and break the
    // page — same posture as a failed config read.
    console.warn("Plausible tracker failed to start", error);
  }
}

/** The instance settings, `ALL_OFF` until the read resolves. */
export function useAppConfig(): AppConfig {
  const [config, setConfig] = useState<AppConfig>(ALL_OFF);

  useEffect(() => {
    let cancelled = false;
    void loadOnce().then((loaded) => {
      if (!cancelled) {
        setConfig(loaded);
        void ensurePlausible(loaded);
      }
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return config;
}

/** Drops the process-wide cache and the tracker guard. Tests only — each case
 * stubs its own fetch, and state from a previous case would mask the new stub. */
export function resetAppConfigForTests(): void {
  cached = undefined;
  trackerStarted = false;
}
