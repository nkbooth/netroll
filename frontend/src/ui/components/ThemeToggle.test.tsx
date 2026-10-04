// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LiveRegion, announce } from "../a11y/LiveRegion";
import { THEME_STORAGE_KEY, initTheme } from "../theme";
import { ThemeToggle } from "./ThemeToggle";

beforeEach(() => {
  localStorage.clear();
  delete document.documentElement.dataset.theme;
  announce("");
  initTheme();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("ThemeToggle", () => {
  it("reflects the active dark theme as pressed", () => {
    render(<ThemeToggle />);

    expect(screen.getByRole("button")).toHaveAttribute("aria-pressed", "true");
  });

  it("switches the theme via keyboard and persists the choice", async () => {
    const user = userEvent.setup();
    render(<ThemeToggle />);

    await user.tab();
    expect(screen.getByRole("button")).toHaveFocus();

    await user.keyboard("{Enter}");

    expect(document.documentElement.dataset.theme).toBe("light");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
    expect(screen.getByRole("button")).toHaveAttribute("aria-pressed", "false");
  });

  it("toggles back with the space key", async () => {
    const user = userEvent.setup();
    render(<ThemeToggle />);

    await user.tab();
    await user.keyboard("{Enter}");
    await user.keyboard(" ");

    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");
    expect(screen.getByRole("button")).toHaveAttribute("aria-pressed", "true");
  });

  it("keeps a second toggle instance in sync", async () => {
    const user = userEvent.setup();
    render(
      <>
        <ThemeToggle />
        <ThemeToggle />
      </>,
    );
    const [first, second] = screen.getAllByRole("button");

    await user.click(first);

    expect(second).toHaveAttribute("aria-pressed", "false");
  });

  it("swaps the icon to give sighted users a state cue beyond aria-pressed", async () => {
    const user = userEvent.setup();
    render(<ThemeToggle />);

    // Dark active (pressed) shows the moon; assert element identity, not the
    // SVG path text.
    expect(screen.getByTestId("theme-icon")).toHaveAttribute(
      "data-icon",
      "moon",
    );

    await user.click(screen.getByRole("button"));

    // Light active (unpressed) shows the sun — a visible difference.
    expect(screen.getByTestId("theme-icon")).toHaveAttribute(
      "data-icon",
      "sun",
    );
    // The accessible label stays constant per ARIA APG (only state changes).
    expect(screen.getByRole("button")).toHaveAccessibleName(/dark mode/i);
  });

  it("names the active theme in visible text, so the cue is not icon-only", () => {
    render(<ThemeToggle />);

    // Dark is active on first paint; the switch says which theme that is
    // rather than leaving the moon glyph to carry it alone.
    expect(screen.getByTestId("theme-current")).toHaveTextContent(/^dark$/i);
  });

  it("renames the visible label when the theme flips", async () => {
    const user = userEvent.setup();
    render(<ThemeToggle />);

    await user.click(screen.getByRole("button"));

    expect(screen.getByTestId("theme-current")).toHaveTextContent(/^light$/i);
    // The ACCESSIBLE name must stay put even though the visible text moved
    // (ARIA APG: only state changes on a toggle button).
    expect(screen.getByRole("button")).toHaveAccessibleName(/dark mode/i);
  });

  it("slides the knob across the track instead of only recoloring it", async () => {
    const user = userEvent.setup();
    render(<ThemeToggle />);

    const knob = screen.getByTestId("theme-knob");
    const darkOffsets = [knob.style.left, knob.style.right];

    await user.click(screen.getByRole("button"));

    // Position is the load-bearing cue: exactly one edge is pinned per state,
    // and it is the opposite edge after the flip.
    expect([knob.style.left, knob.style.right]).not.toEqual(darkOffsets);
    expect(darkOffsets.filter((offset) => offset !== "")).toHaveLength(1);
    expect(
      [knob.style.left, knob.style.right].filter((offset) => offset !== ""),
    ).toHaveLength(1);
  });

  it("announces politely when the theme choice cannot be persisted", async () => {
    const user = userEvent.setup();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("storage blocked");
    });
    render(
      <>
        <ThemeToggle />
        <LiveRegion />
      </>,
    );
    expect(screen.getByRole("status").textContent).toBe("");

    await user.click(screen.getByRole("button"));

    // The previously-silent persistence failure now reaches AT politely —
    // assert a message landed, not its exact prose.
    expect(screen.getByRole("status").textContent).not.toBe("");
  });

  it("clears a stale persistence-failure announcement once a later toggle succeeds", async () => {
    const user = userEvent.setup();
    const setItemSpy = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementationOnce(() => {
        throw new Error("storage blocked");
      });
    render(
      <>
        <ThemeToggle />
        <LiveRegion />
      </>,
    );

    await user.click(screen.getByRole("button"));
    expect(screen.getByRole("status").textContent).not.toBe("");

    // Persistence recovers (mock only threw once) — the stale failure
    // message must not linger in the live region past the point it's true.
    setItemSpy.mockRestore();
    await user.click(screen.getByRole("button"));

    expect(screen.getByRole("status").textContent).toBe("");
  });

  it("follows a theme change written by another tab", () => {
    render(<ThemeToggle />);

    act(() => {
      window.dispatchEvent(
        new StorageEvent("storage", {
          key: THEME_STORAGE_KEY,
          newValue: "light",
        }),
      );
    });

    expect(document.documentElement.dataset.theme).toBe("light");
    expect(screen.getByRole("button")).toHaveAttribute("aria-pressed", "false");
  });
});
