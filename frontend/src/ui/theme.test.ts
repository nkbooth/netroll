// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { beforeEach, describe, expect, it } from "vitest";

import {
  THEME_STORAGE_KEY,
  currentTheme,
  initTheme,
  setTheme,
  subscribeToTheme,
  toggleTheme,
} from "./theme";

beforeEach(() => {
  localStorage.clear();
  delete document.documentElement.dataset.theme;
});

describe("initTheme", () => {
  it("defaults to dark with no stored preference and stamps <html>", () => {
    const theme = initTheme();

    expect(theme).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("restores a stored light preference (persistence across reloads)", () => {
    localStorage.setItem(THEME_STORAGE_KEY, "light");

    expect(initTheme()).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");
  });

  it("falls back to dark on a garbage stored value", () => {
    localStorage.setItem(THEME_STORAGE_KEY, "hotdog-stand");

    expect(initTheme()).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });
});

describe("toggleTheme", () => {
  it("switches dark to light, stamps the attribute, and persists the choice", () => {
    initTheme();

    const next = toggleTheme();

    expect(next).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  });

  it("round-trips back to dark and persists that too", () => {
    initTheme();
    toggleTheme();

    expect(toggleTheme()).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");
  });

  it("survives a simulated reload after toggling", () => {
    initTheme();
    toggleTheme();
    delete document.documentElement.dataset.theme;

    expect(initTheme()).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");
  });
});

describe("setTheme", () => {
  it("stamps and persists an explicit theme", () => {
    setTheme("light");

    expect(document.documentElement.dataset.theme).toBe("light");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  });
});

describe("cross-tab sync", () => {
  const storageEvent = (key: string, newValue: string): StorageEvent =>
    new StorageEvent("storage", { key, newValue });

  it("follows a theme change written by another tab", () => {
    initTheme();

    window.dispatchEvent(storageEvent(THEME_STORAGE_KEY, "light"));

    expect(document.documentElement.dataset.theme).toBe("light");
  });

  it("ignores storage events for foreign keys", () => {
    initTheme();

    window.dispatchEvent(storageEvent("unrelated-key", "light"));

    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("ignores garbage theme values from another tab", () => {
    initTheme();

    window.dispatchEvent(storageEvent(THEME_STORAGE_KEY, "hotdog-stand"));

    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("notifies subscribers when the theme changes", () => {
    initTheme();
    const seen: string[] = [];
    const unsubscribe = subscribeToTheme(() => {
      seen.push(currentTheme());
    });

    setTheme("light");
    expect(seen).toEqual(["light"]);

    unsubscribe();
    setTheme("dark");
    expect(seen).toEqual(["light"]);
  });
});
