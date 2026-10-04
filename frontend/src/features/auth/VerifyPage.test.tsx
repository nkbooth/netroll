// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { VerifyPage } from "./VerifyPage";
import { messageForProblemType } from "../../errors/problemMessages";
import { expectNoAxeViolations } from "../../test/axe";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

function renderVerify(fetchMock: ReturnType<typeof vi.fn>) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(
    [
      { path: "/", element: <div data-testid="home" /> },
      { path: "/auth/verify", element: <VerifyPage /> },
      { path: "/sign-in", element: <div data-testid="sign-in" /> },
      { path: "/consent", element: <div data-testid="consent" /> },
    ],
    { initialEntries: ["/auth/verify?token=raw-token-abc"] },
  );
  render(<RouterProvider router={router} />);
  return router;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("VerifyPage", () => {
  it("does NOT consume the token on render — only on explicit action", () => {
    const fetchMock = vi.fn();

    renderVerify(fetchMock);

    // Email scanners prefetch the page; rendering must never burn the
    // single-use token.
    expect(fetchMock).not.toHaveBeenCalled();
    expect(screen.getByRole("button")).toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations on the verify prompt", async () => {
    renderVerify(vi.fn());

    await expectNoAxeViolations(document.body);
  });

  it("consumes on click and redirects home when no consent is pending", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(201, {
        id: "1",
        email: "op@example.com",
        emailVerifiedAt: "2026-07-15T00:00:00Z",
        consentRequired: false,
        requiredTermsVersion: "2026-07-15",
      }),
    );

    renderVerify(fetchMock);
    await user.click(screen.getByRole("button"));

    expect(await screen.findByTestId("home")).toBeInTheDocument();
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/sessions");
    expect(JSON.parse(init.body as string)).toEqual({
      token: "raw-token-abc",
    });
  });

  it("steers a consent-requiring sign-in to the gate first", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(201, {
        id: "1",
        email: "op@example.com",
        emailVerifiedAt: "2026-07-15T00:00:00Z",
        consentRequired: true,
        requiredTermsVersion: "2026-07-15",
      }),
    );

    renderVerify(fetchMock);
    await user.click(screen.getByRole("button"));

    expect(await screen.findByTestId("consent")).toBeInTheDocument();
  });

  it("wraps its content in the shared Card chrome", async () => {
    renderVerify(vi.fn());

    const card = screen
      .getByRole("heading", { name: /almost there/i })
      .closest("div");
    expect(card).toHaveStyle({
      background: "var(--surface)",
      borderRadius: "var(--rounded-xl)",
    });
  });

  it("shows the mapped message and a path back to sign-in on failure", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(401, { type: "/errors/magic-link-expired", status: 401 }),
      );

    renderVerify(fetchMock);
    await user.click(screen.getByRole("button"));

    // The rendered copy must be exactly what the slug map produces — the
    // mapping, not the literal string, is the contract.
    const expected = messageForProblemType("/errors/magic-link-expired");
    expect(await screen.findByText(expected)).toBeInTheDocument();
    const backLink = screen.getByRole("link");
    expect(backLink).toHaveAttribute("href", "/sign-in");
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy — a test-owned fixture against the computed map copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(401, {
        type: "/errors/magic-link-expired",
        status: 401,
        detail,
      }),
    );

    renderVerify(fetchMock);
    await user.click(screen.getByRole("button"));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/magic-link-expired"),
    );
  });

});
