// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SignInPage } from "./SignInPage";
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

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("SignInPage", () => {
  it("has a labeled email input (keyboard/AT operable)", () => {
    vi.stubGlobal("fetch", vi.fn());
    render(<SignInPage />);

    const input = screen.getByLabelText(/email/i);
    expect(input).toHaveAttribute("type", "email");
  });

  it("has no WCAG 2.1 AA violations", async () => {
    vi.stubGlobal("fetch", vi.fn());
    const { container } = render(<SignInPage />);

    await expectNoAxeViolations(container);
  });

  it("requests a link for the entered address and swaps to the sent state", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<SignInPage />);
    await user.type(screen.getByLabelText(/email/i), "op@example.com");
    await user.click(
      screen.getByRole("button", { name: /email me a sign-in link/i }),
    );

    // The sent state names the address the link went to and retires the form.
    expect(await screen.findByText(/op@example\.com/)).toBeInTheDocument();
    expect(screen.queryByLabelText(/email/i)).not.toBeInTheDocument();

    const magicLinkCall = fetchMock.mock.calls.find(
      ([u]) => u === "/api/magic-links",
    ) as [string, RequestInit];
    expect(magicLinkCall).toBeDefined();
    // The submission carries the email plus the (empty) honeypot; no token
    // here since the mount fetch returns no form token in this stub.
    expect(JSON.parse(magicLinkCall[1].body as string)).toEqual({
      email: "op@example.com",
      hpField: "",
    });
  });

  it("confirms the normalized address the backend will actually mail", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<SignInPage />);
    await user.type(screen.getByLabelText(/email/i), "Op@Example.COM");
    await user.click(
      screen.getByRole("button", { name: /email me a sign-in link/i }),
    );

    // The confirmation must name the mailbox the mail actually goes to,
    // not the raw casing the user happened to type.
    expect(await screen.findByText(/op@example\.com/)).toBeInTheDocument();
    expect(screen.queryByText(/Op@Example\.COM/)).not.toBeInTheDocument();

    const magicLinkCall = fetchMock.mock.calls.find(
      ([u]) => u === "/api/magic-links",
    ) as [string, RequestInit];
    expect(JSON.parse(magicLinkCall[1].body as string)).toEqual({
      email: "op@example.com",
      hpField: "",
    });
  });

  it("shows the mapped message when the request is rate-limited", async () => {
    const user = userEvent.setup();
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          jsonResponse(429, { type: "/errors/rate-limited", status: 429 }),
        ),
    );

    render(<SignInPage />);
    await user.type(screen.getByLabelText(/email/i), "op@example.com");
    await user.click(
      screen.getByRole("button", { name: /email me a sign-in link/i }),
    );

    const expected = messageForProblemType("/errors/rate-limited");
    expect(await screen.findByText(expected)).toBeInTheDocument();
    // The form stays available so a new request is possible later.
    expect(screen.getByLabelText(/email/i)).toBeInTheDocument();
  });

  it("disables submit until the mount form-token fetch resolves (fast-submit race guard)", async () => {
    // A submit racing ahead of the mount-time token fetch would read as a
    // missing token — which the server treats as a bot when mitigation is
    // enabled. Gating submit on the fetch resolving
    // (not just its value) closes that race.
    let resolveTokenFetch: (response: Response) => void = () => {};
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      if (input === "/api/form-tokens") {
        return new Promise<Response>((resolve) => {
          resolveTokenFetch = resolve;
        });
      }
      return Promise.resolve(new Response(null, { status: 202 }));
    });
    vi.stubGlobal("fetch", fetchMock);

    render(<SignInPage />);
    const button = screen.getByRole("button", {
      name: /email me a sign-in link/i,
    });
    expect(button).toBeDisabled();

    resolveTokenFetch(jsonResponse(200, { formToken: null }));
    await waitFor(() => expect(button).not.toBeDisabled());
  });

  it("wraps the sign-in form in the shared Card chrome", () => {
    vi.stubGlobal("fetch", vi.fn());
    render(<SignInPage />);

    const card = screen.getByRole("heading", { name: /sign in to netroll/i })
      .closest("div");
    expect(card).toHaveStyle({
      background: "var(--surface)",
      borderRadius: "var(--rounded-xl)",
    });
  });

  it("offers a disabled Continue with Google option below a divider", () => {
    vi.stubGlobal("fetch", vi.fn());
    render(<SignInPage />);

    const googleButton = screen.getByRole("button", {
      name: /continue with google/i,
    });
    expect(googleButton).toBeDisabled();
  });

  it("shows a Terms/Privacy footnote on the sign-in form", () => {
    vi.stubGlobal("fetch", vi.fn());
    render(<SignInPage />);

    expect(
      screen.getByText(/terms/i, { selector: "p" }),
    ).toBeInTheDocument();
  });

  it("offers a resend affordance and a way back to a different email on the sent state", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<SignInPage />);
    await user.type(screen.getByLabelText(/email/i), "op@example.com");
    await user.click(
      screen.getByRole("button", { name: /email me a sign-in link/i }),
    );
    await screen.findByText(/op@example\.com/);

    const callsBeforeResend = fetchMock.mock.calls.filter(
      ([u]) => u === "/api/magic-links",
    ).length;
    await user.click(screen.getByRole("button", { name: /send it again/i }));
    await waitFor(() => {
      const callsAfterResend = fetchMock.mock.calls.filter(
        ([u]) => u === "/api/magic-links",
      ).length;
      expect(callsAfterResend).toBe(callsBeforeResend + 1);
    });

    await user.click(
      screen.getByRole("button", { name: /use a different email/i }),
    );

    // Back on the form, ready for a fresh address — the old one is gone.
    expect(await screen.findByLabelText(/email/i)).toHaveValue("");
    expect(screen.queryByText(/op@example\.com/)).not.toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations on the sent state", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();

    const { container } = render(<SignInPage />);
    await user.type(screen.getByLabelText(/email/i), "op@example.com");
    await user.click(
      screen.getByRole("button", { name: /email me a sign-in link/i }),
    );
    await screen.findByText(/op@example\.com/);

    await expectNoAxeViolations(container);
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces that resolve through `messageForProblem`.
    // `/errors/validation`
    // covers every extractor rejection and every `ApiError::Validation` site,
    // so its mapped copy is necessarily generic; `detail` is the only part of
    // the response that can name WHICH field failed and what would make it
    // acceptable. What this test guards is the ordering AT THIS SURFACE — a
    // data-flow property, not a piece of copy — so a regression that reverts
    // this one file is caught here rather than only at the resolver.
    const user = userEvent.setup();
    const detail = "the field-naming answer the server sent";
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        jsonResponse(400, {
          type: "/errors/validation",
          status: 400,
          detail,
        }),
      ),
    );

    render(<SignInPage />);
    await user.type(screen.getByLabelText(/email/i), "op@example.com");
    await user.click(
      screen.getByRole("button", { name: /email me a sign-in link/i }),
    );

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/validation"),
    );
  });

  it("falls back to the slug map when the problem carries no detail", async () => {
    // The other half of the ordering: with nothing to prefer, the curated map
    // is still what the reader gets — so the preference above cannot be
    // satisfied by dropping the fallback.
    const user = userEvent.setup();
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          jsonResponse(400, { type: "/errors/validation", status: 400 }),
        ),
    );

    render(<SignInPage />);
    await user.type(screen.getByLabelText(/email/i), "op@example.com");
    await user.click(
      screen.getByRole("button", { name: /email me a sign-in link/i }),
    );

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(messageForProblemType("/errors/validation"));
  });
});
