// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { ConfirmEmailChangePage } from "./ConfirmEmailChangePage";
import { messageForProblemType } from "../../errors/problemMessages";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

function renderConfirm(fetchMock: ReturnType<typeof vi.fn>) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(
    [
      { path: "/auth/confirm-email-change", element: <ConfirmEmailChangePage /> },
      { path: "/sign-in", element: <div data-testid="sign-in" /> },
      { path: "/profile", element: <div data-testid="profile" /> },
    ],
    { initialEntries: ["/auth/confirm-email-change?token=raw-token-abc"] },
  );
  render(<RouterProvider router={router} />);
  return router;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ConfirmEmailChangePage", () => {
  it("does NOT consume the token on render — only on explicit action", () => {
    const fetchMock = vi.fn();

    renderConfirm(fetchMock);

    // Email scanners prefetch links; rendering must never burn the token.
    expect(fetchMock).not.toHaveBeenCalled();
    expect(screen.getByRole("button")).toBeInTheDocument();
  });

  it("confirms on click, POSTing the token from the query string", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { email: "new@example.com" }));

    renderConfirm(fetchMock);
    await user.click(screen.getByRole("button"));

    const [url, init] = (await vi.waitFor(() => {
      expect(fetchMock).toHaveBeenCalled();
      return fetchMock.mock.calls[0];
    })) as [string, RequestInit];
    expect(url).toBe("/api/email-changes");
    expect(JSON.parse(init.body as string)).toEqual({ token: "raw-token-abc" });
  });

  it("shows the new address and a link to sign in again on success", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { email: "new@example.com" }));

    renderConfirm(fetchMock);
    await user.click(screen.getByRole("button"));

    expect(await screen.findByText(/new@example\.com/)).toBeInTheDocument();
    // Signed out everywhere — the only path forward is a fresh sign-in.
    const link = screen.getByRole("link");
    expect(link).toHaveAttribute("href", "/sign-in");
  });

  it("shows the mapped message and a path back to the profile on failure", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(401, {
          type: "/errors/email-change-expired",
          status: 401,
        }),
      );

    renderConfirm(fetchMock);
    await user.click(screen.getByRole("button"));

    const expected = messageForProblemType("/errors/email-change-expired");
    expect(await screen.findByText(expected)).toBeInTheDocument();
    const link = screen.getByRole("link");
    expect(link).toHaveAttribute("href", "/profile");
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(401, {
        type: "/errors/email-change-expired",
        status: 401,
        detail,
      }),
    );

    renderConfirm(fetchMock);
    await user.click(screen.getByRole("button"));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/email-change-expired"),
    );
  });

});
