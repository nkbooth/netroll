// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ProblemError } from "../../features/auth/authApi";
import { messageForProblemType } from "../../errors/problemMessages";
import { expectNoAxeViolations } from "../../test/axe";
import { FavoriteToggle } from "./FavoriteToggle";

describe("FavoriteToggle", () => {
  it("reflects favorited state via aria-pressed and a filled star", () => {
    render(<FavoriteToggle favorited={true} onToggle={vi.fn()} />);
    const button = screen.getByRole("button");
    expect(button).toHaveAttribute("aria-pressed", "true");
    // State is conveyed by icon shape, not color alone.
    expect(screen.getByTestId("favorite-icon")).toHaveAttribute(
      "data-icon",
      "star-filled",
    );
  });

  it("shows an outline star with aria-pressed=false when not favorited", () => {
    render(<FavoriteToggle favorited={false} onToggle={vi.fn()} />);
    expect(screen.getByRole("button")).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByTestId("favorite-icon")).toHaveAttribute(
      "data-icon",
      "star-outline",
    );
  });

  it("calls onToggle with the next state on click", async () => {
    const onToggle = vi.fn().mockResolvedValue(undefined);
    render(<FavoriteToggle favorited={false} onToggle={onToggle} />);

    await userEvent.click(screen.getByRole("button"));
    // Requesting to favorite (from not-favorited → true) — the parent performs
    // the write and reflects the new state (await server, then reflect).
    expect(onToggle).toHaveBeenCalledWith(true);
  });

  it("surfaces the mapped problem and leaves state unchanged on failure", async () => {
    const onToggle = vi
      .fn()
      .mockRejectedValue(new ProblemError({ type: "/errors/rate-limited", status: 429 }));
    render(<FavoriteToggle favorited={false} onToggle={onToggle} />);

    await userEvent.click(screen.getByRole("button"));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(messageForProblemType("/errors/rate-limited"));
    // The optimistic state was never shown: aria-pressed stays false because
    // the parent never updated it.
    expect(screen.getByRole("button")).toHaveAttribute("aria-pressed", "false");
  });

  it("has no accessibility violations", async () => {
    const { container } = render(
      <FavoriteToggle favorited={false} onToggle={vi.fn()} />,
    );
    await waitFor(() => expect(screen.getByRole("button")).toBeInTheDocument());
    await expectNoAxeViolations(container);
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // The assertion is the preference ORDER — a test-owned fixture
    // against the computed map copy. Neither half is a literal sentence.
    const detail = "the field-naming answer the server sent";
    const onToggle = vi
      .fn()
      .mockRejectedValue(
        new ProblemError({ type: "/errors/rate-limited", status: 429, detail }),
      );
    render(<FavoriteToggle favorited={false} onToggle={onToggle} />);

    await userEvent.click(screen.getByRole("button"));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/rate-limited"),
    );
  });

});
