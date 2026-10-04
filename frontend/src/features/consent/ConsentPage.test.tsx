// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { UserEvent } from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { ConsentPage } from "./ConsentPage";
import {
  GENERIC_PROBLEM_MESSAGE,
  messageForProblemType,
} from "../../errors/problemMessages";
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

function meResponse(consentRequired: boolean, version = "2026-07-15") {
  return jsonResponse(200, {
    id: "1",
    email: "op@example.com",
    emailVerifiedAt: "2026-07-15T00:00:00Z",
    consentRequired,
    requiredTermsVersion: version,
  });
}

function renderConsent(
  fetchMock: ReturnType<typeof vi.fn>,
  initialEntries: Parameters<typeof createMemoryRouter>[1] = {
    initialEntries: ["/consent"],
  },
) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(
    [
      { path: "/", element: <div data-testid="home" /> },
      { path: "/consent", element: <ConsentPage /> },
      { path: "/sign-in", element: <div data-testid="sign-in" /> },
      { path: "/nets/42", element: <div data-testid="destination" /> },
    ],
    initialEntries,
  );
  render(<RouterProvider router={router} />);
  return router;
}

/**
 * Both consent checkboxes must be ticked before "Agree & continue" is
 * enabled — checks them, then clicks continue. Waits for the checkboxes to
 * render first, so it's safe to call as soon as the page mounts.
 */
async function agreeToTerms(user: UserEvent): Promise<void> {
  await user.click(
    await screen.findByRole("checkbox", { name: /terms of service/i }),
  );
  await user.click(
    screen.getByRole("checkbox", { name: /publicly visible/i }),
  );
  await user.click(screen.getByRole("button", { name: /agree/i }));
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ConsentPage", () => {
  it("records consent to the server-supplied version and returns to the destination", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(true, "2026-07-15"))
      .mockResolvedValueOnce(jsonResponse(201, {}));

    renderConsent(fetchMock, {
      initialEntries: [
        { pathname: "/consent", state: { returnTo: "/nets/42" } },
      ],
    });

    await agreeToTerms(user);

    // Gate-and-return: acceptance lands on the carried destination.
    expect(await screen.findByTestId("destination")).toBeInTheDocument();

    // The echoed version is the one the server supplied via /me — the
    // client never hardcodes it.
    const consentCall = fetchMock.mock.calls.find(
      ([url]) => url === "/api/consents",
    ) as [string, RequestInit];
    expect(consentCall).toBeDefined();
    expect(consentCall[1].method).toBe("POST");
    expect(JSON.parse(consentCall[1].body as string)).toEqual({
      termsVersion: "2026-07-15",
    });
  });

  it("defaults the return destination to the landing page", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(true))
      .mockResolvedValueOnce(jsonResponse(201, {}));

    renderConsent(fetchMock);
    await agreeToTerms(user);

    expect(await screen.findByTestId("home")).toBeInTheDocument();
  });

  it("keeps Agree & continue disabled until both checkboxes are checked", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(true));

    renderConsent(fetchMock);

    const continueButton = await screen.findByRole("button", {
      name: /agree/i,
    });
    expect(continueButton).toBeDisabled();

    const termsCheckbox = screen.getByRole("checkbox", {
      name: /terms of service/i,
    });
    const visibilityCheckbox = screen.getByRole("checkbox", {
      name: /publicly visible/i,
    });

    await user.click(termsCheckbox);
    expect(continueButton).toBeDisabled();

    await user.click(visibilityCheckbox);
    expect(continueButton).not.toBeDisabled();

    // Unchecking either box re-locks the gate.
    await user.click(termsCheckbox);
    expect(continueButton).toBeDisabled();
  });

  it("exposes exactly two consent checkboxes — no marketing opt-in", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(true));

    renderConsent(fetchMock);
    await screen.findByRole("button", { name: /agree/i });

    // Locked product decision: terms + public-visibility only, no third
    // marketing-email opt-in checkbox.
    expect(screen.getAllByRole("checkbox")).toHaveLength(2);
  });

  it("re-fetches the required version on a version mismatch instead of dead-ending", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(true, "2026-07-15"))
      .mockResolvedValueOnce(
        jsonResponse(409, {
          type: "/errors/consent-version-mismatch",
          status: 409,
        }),
      )
      .mockResolvedValueOnce(meResponse(true, "2026-09-01"));

    renderConsent(fetchMock);
    await agreeToTerms(user);

    // The stale gate recovers by re-reading /me to learn the new version.
    await waitFor(() => {
      const meCalls = fetchMock.mock.calls.filter(
        ([url]) => url === "/api/accounts/me",
      );
      expect(meCalls.length).toBe(2);
    });

    // A second agree consents to the NEW version. The checkboxes are still
    // checked from before, so only the button needs another click.
    fetchMock.mockResolvedValueOnce(jsonResponse(201, {}));
    await user.click(screen.getByRole("button", { name: /agree/i }));
    await waitFor(() => {
      const consentCalls = fetchMock.mock.calls.filter(
        ([url]) => url === "/api/consents",
      );
      expect(consentCalls.length).toBe(2);
    });
    const lastConsent = fetchMock.mock.calls
      .filter(([url]) => url === "/api/consents")
      .at(-1) as [string, RequestInit];
    expect(JSON.parse(lastConsent[1].body as string)).toEqual({
      termsVersion: "2026-09-01",
    });
  });

  it("keeps I agree disabled until the post-mismatch account refresh resolves", async () => {
    const user = userEvent.setup();
    let resolveMeRefetch: (response: Response) => void = () => {};
    const meRefetchPending = new Promise<Response>((resolve) => {
      resolveMeRefetch = resolve;
    });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(true, "2026-07-15"))
      .mockResolvedValueOnce(
        jsonResponse(409, {
          type: "/errors/consent-version-mismatch",
          status: 409,
        }),
      )
      .mockImplementationOnce(() => meRefetchPending);

    renderConsent(fetchMock);
    await agreeToTerms(user);

    // reset() returns the request to idle as soon as the mismatch lands, but
    // the stale requiredVersion closure is still in play until the /me
    // refetch it kicked off actually resolves. A fast re-click in that window
    // must not be able to resubmit the stale version.
    await waitFor(() => {
      expect(screen.getByRole("button", { name: /agree/i })).toBeDisabled();
    });

    resolveMeRefetch(meResponse(true, "2026-09-01"));

    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: /agree/i }),
      ).not.toBeDisabled();
    });
  });

  it("always offers sign-out as the decline path", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(true))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));

    renderConsent(fetchMock);
    await user.click(
      await screen.findByRole("button", { name: /sign out/i }),
    );

    // Declining is never a trap: the session is revoked server-side. It
    // must not require the consent checkboxes to be checked first.
    const signOutCall = fetchMock.mock.calls.find(
      ([url]) => url === "/api/sessions/current",
    ) as [string, RequestInit];
    expect(signOutCall).toBeDefined();
    expect(signOutCall[1].method).toBe("DELETE");
    expect(await screen.findByTestId("home")).toBeInTheDocument();
  });

  it("disables Sign out while an I agree submission is in flight", async () => {
    const user = userEvent.setup();
    let resolveConsent: (response: Response) => void = () => {};
    const pending = new Promise<Response>((resolve) => {
      resolveConsent = resolve;
    });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(true))
      .mockImplementationOnce(() => pending);

    renderConsent(fetchMock);
    const signOut = await screen.findByRole("button", { name: /sign out/i });
    expect(signOut).not.toBeDisabled();

    await agreeToTerms(user);

    // In flight: sign-out must not race consent-recording against session
    // revocation.
    expect(signOut).toBeDisabled();

    resolveConsent(jsonResponse(201, {}));
    await screen.findByTestId("home");
  });

  it("has no WCAG 2.1 AA violations on the consent gate", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(true));

    renderConsent(fetchMock);
    await screen.findByRole("button", { name: /agree/i });

    await expectNoAxeViolations(document.body);
  });

  it("redirects an already-consented account straight through", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false));

    renderConsent(fetchMock);

    // No gate once consent is recorded.
    expect(await screen.findByTestId("home")).toBeInTheDocument();
  });

  it("sends the signed-out to sign-in rather than showing a gate", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }),
      );

    renderConsent(fetchMock);

    expect(await screen.findByTestId("sign-in")).toBeInTheDocument();
  });

  it("offers a retry instead of signing out when the account check fails with a server error", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(500, { type: "/errors/internal", status: 500 }),
      );

    renderConsent(fetchMock);

    // A transient server error is not the same as being signed out — it
    // must never bounce an authenticated user to /sign-in.
    await screen.findByRole("alert");
    expect(screen.queryByTestId("sign-in")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /try again/i })).toBeInTheDocument();
  });

  it("returns to sign-in when the session expires while submitting consent", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(true))
      .mockResolvedValueOnce(
        jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }),
      );

    renderConsent(fetchMock);
    await agreeToTerms(user);

    // A session that expired between page-load and submit must not leave
    // the user stranded on the gate with no way back.
    expect(await screen.findByTestId("sign-in")).toBeInTheDocument();
  });

  it("wraps the gate in the shared Card chrome", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(true));

    renderConsent(fetchMock);
    const card = (
      await screen.findByRole("heading", { name: /before you check in/i })
    ).closest("div");

    expect(card).toHaveStyle({
      background: "var(--surface)",
      borderRadius: "var(--rounded-xl)",
    });
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(true))
      .mockResolvedValueOnce(
        jsonResponse(429, {
          type: "/errors/rate-limited",
          status: 429,
          detail,
        }),
      );

    renderConsent(fetchMock);
    await agreeToTerms(user);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/rate-limited"),
    );
  });

  it("shows the detail of a failed account check instead of discarding the problem", async () => {
    // `loadAccount` once swallowed the problem in a bare `catch` and rendered
    // the generic fallback, so a server that explained itself was never heard
    // on this surface.
    const detail = "the field-naming answer the server sent";
    const fetchMock = vi.fn().mockResolvedValueOnce(
      jsonResponse(500, {
        type: "/errors/internal",
        status: 500,
        detail,
      }),
    );

    renderConsent(fetchMock);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(GENERIC_PROBLEM_MESSAGE);
  });

});
