// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { Theme } from "./tokens/tokens";

/**
 * localStorage key holding the user's explicit theme choice. Mirrored by the
 * FOUC-guard inline script in index.html — change both together (a weld test
 * in theme-fouc.test.ts fails if they drift).
 */
export const THEME_STORAGE_KEY = "netroll-theme";

type ThemeListener = () => void;

const listeners = new Set<ThemeListener>();

const applyTheme = (theme: Theme): void => {
  document.documentElement.dataset.theme = theme;
  for (const listener of listeners) {
    listener();
  }
};

/**
 * Subscribe to theme changes (any tab, any caller). Returns an unsubscribe
 * function; the signature is `useSyncExternalStore`-compatible.
 */
export function subscribeToTheme(listener: ThemeListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

const onStorageChange = (event: StorageEvent): void => {
  // Another tab persisted a theme choice — mirror it here. Do not re-persist:
  // storage already holds the value, and writing back would echo the event.
  if (event.key === THEME_STORAGE_KEY && isTheme(event.newValue)) {
    applyTheme(event.newValue);
  }
};

const DEFAULT_THEME: Theme = "dark";

const isTheme = (value: unknown): value is Theme =>
  value === "dark" || value === "light";

const readStoredTheme = (): Theme | null => {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY);
    return isTheme(stored) ? stored : null;
  } catch {
    // Storage can be unavailable (private mode, blocked cookies). The app
    // must still boot, so treat it as "no stored preference".
    return null;
  }
};

/**
 * Resolve the theme on boot: stored preference if valid, otherwise the dark
 * product default (never `prefers-color-scheme`), and stamp `data-theme` on
 * `<html>`. Returns the resolved theme.
 */
export function initTheme(): Theme {
  const theme = readStoredTheme() ?? DEFAULT_THEME;
  applyTheme(theme);
  // Cross-tab sync. Re-adding the same handler reference is a DOM no-op, so
  // repeated initTheme calls (tests) never double-subscribe.
  window.addEventListener("storage", onStorageChange);
  return theme;
}

/** The theme currently stamped on `<html>`, defaulting to dark. */
export function currentTheme(): Theme {
  const value = document.documentElement.dataset.theme;
  return isTheme(value) ? value : DEFAULT_THEME;
}

/**
 * Stamp `data-theme` and persist the choice across reloads. Returns whether
 * persistence succeeded: the in-page theme always switches, but `false`
 * surfaces a blocked/full storage so callers can tell the user the choice
 * won't survive a reload (previously this failure was swallowed silently).
 */
export function setTheme(theme: Theme): boolean {
  applyTheme(theme);
  try {
    localStorage.setItem(THEME_STORAGE_KEY, theme);
    return true;
  } catch {
    // Persistence is best-effort — the in-page theme has already switched.
    return false;
  }
}

/** Flip between dark and light. Returns the newly active theme. */
export function toggleTheme(): Theme {
  const next: Theme = currentTheme() === "dark" ? "light" : "dark";
  setTheme(next);
  return next;
}
