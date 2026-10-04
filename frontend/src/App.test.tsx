// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import { expectNoAxeViolations } from "./test/axe";
import { resetAppConfigForTests } from "./features/appConfig/useAppConfig";
import { THEME_STORAGE_KEY } from "./ui/theme";

beforeEach(() => {
  localStorage.clear();
  document.documentElement.dataset.theme = "dark";
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("App shell", () => {
  it("exposes the theme toggle as the only interactive control", () => {
    render(<App />);

    expect(screen.getAllByRole("button")).toHaveLength(1);
  });

  it("exposes the wordmark as the top-level document heading", () => {
    render(<App />);

    // Assistive tech needs a real h1, not a styled <p> — assert role/level,
    // not the byline prose.
    const heading = screen.getByRole("heading", { level: 1 });
    expect(heading).toHaveAccessibleName(/netroll/i);
  });

  it("sets the brand logomark beside the wordmark", () => {
    render(<App />);

    const heading = screen.getByRole("heading", { level: 1 });
    // The mark lives inside the heading so the two travel together, and stays
    // decorative — the heading text is the accessible name.
    expect(heading).toContainElement(screen.getByTestId("logomark"));
    expect(heading).toHaveAccessibleName(/^netroll/i);
  });

  it("takes the wordmark back to the discovery root", () => {
    render(<App />);

    // Discovery is the app root; the wordmark is the way back to it
    // from any surface, since only sessions carry a breadcrumb.
    expect(screen.getByRole("link", { name: /netroll/i })).toHaveAttribute(
      "href",
      "/",
    );
  });

  it("aligns header and footer chrome to the same measure as page content", () => {
    render(<App />);

    // Full-bleed bars, content-width interiors: chrome that ran edge-to-edge
    // would leave the wordmark floating away from the content below it.
    for (const landmark of ["banner", "contentinfo"] as const) {
      const inner = screen.getByRole(landmark).firstElementChild;
      expect(inner).toHaveStyle({ maxWidth: "1200px" });
    }
  });

  it("offers no support link when no Ko-fi username is configured", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            plausibleDomain: null,
            plausibleScriptHost: null,
            kofiUsername: null,
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      ),
    );
    resetAppConfigForTests();

    render(<App />);
    await screen.findByRole("link", { name: /report abuse/i });

    // Default posture: nothing asks a visitor for money.
    expect(
      screen.queryByRole("link", { name: /support/i }),
    ).not.toBeInTheDocument();
  });

  it("links to Ko-fi from the footer once a username is configured", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation((url: string) =>
        Promise.resolve(
          new Response(
            JSON.stringify(
              url === "/api/app-config"
                ? {
                    plausibleDomain: null,
                    plausibleScriptHost: null,
                    kofiUsername: "n1cck",
                  }
                : { type: "/errors/unauthenticated", status: 401 },
            ),
            {
              status: url === "/api/app-config" ? 200 : 401,
              headers: { "content-type": "application/json" },
            },
          ),
        ),
      ),
    );
    resetAppConfigForTests();

    render(<App />);

    const support = await screen.findByRole("link", { name: /support/i });
    // A plain link to the operator's own page: no third-party script, no
    // iframe, nothing that can track the visitor.
    expect(support).toHaveAttribute("href", "https://ko-fi.com/n1cck");
    expect(support).toHaveAttribute("rel", expect.stringContaining("noopener"));
    expect(support.closest("footer")).not.toBeNull();
  });

  it("links to the documentation from the footer", () => {
    render(<App />);

    const docs = screen.getByRole("link", { name: /docs/i });
    // The docs are served by the backend out of the SPA bundle dir
    // (/app/static/docs), NOT by a client route — so this must stay a plain
    // href that leaves the SPA. A React Router navigation would hit the
    // catch-all and render the app's not-found page instead.
    expect(docs).toHaveAttribute("href", "/docs/");
    expect(docs.closest("footer")).not.toBeNull();
  });

  it("keeps the docs link available without an account", () => {
    // Documentation is auth-independent: a visitor evaluating NetRoll, or an
    // operator locked out mid-net, both need it. It must not sit behind the
    // signed-in chrome the way My Nets and Profile do.
    vi.stubGlobal("fetch", vi.fn().mockReturnValue(new Promise(() => {})));

    render(<App />);

    expect(screen.getByRole("link", { name: /docs/i })).toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations in the app shell", async () => {
    const { container } = render(<App />);

    await expectNoAxeViolations(container);
  });

  it("switches and persists the theme from within the shell", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button"));

    expect(document.documentElement.dataset.theme).toBe("light");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  });

  it("offers neither sign-in nor an account while the session check is pending", () => {
    // A pending /api/accounts/me must not flash "Sign in" at an
    // already-authenticated user.
    vi.stubGlobal(
      "fetch",
      vi.fn().mockReturnValue(new Promise(() => {})),
    );

    render(<App />);

    // The persistent public "Report abuse" footer link is auth-independent, so
    // scope this to the auth-area sign-in link that must not flash while pending.
    expect(
      screen.queryByRole("link", { name: /sign in/i }),
    ).not.toBeInTheDocument();
  });

  it("keeps the session visible and reports the failure when sign-out fails", async () => {
    const user = userEvent.setup();
    const account = {
      id: "1",
      email: "op@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(JSON.stringify(account), { status: 200 }),
      )
      .mockRejectedValueOnce(new TypeError("network down"));
    vi.stubGlobal("fetch", fetchMock);

    render(<App />);
    expect(await screen.findByText(account.email)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /sign out/i }));

    // The server session was never revoked and the cookie is still live —
    // pretending to be signed out would be a lie the next reload exposes.
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(screen.getByText(account.email)).toBeInTheDocument();
  });

  it("offers sign-in when no session exists", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({ type: "/errors/unauthenticated", status: 401 }),
          { status: 401 },
        ),
      ),
    );

    render(<App />);

    const signIn = await screen.findByRole("link", { name: /sign in/i });
    expect(signIn).toHaveAttribute("href", "/sign-in");
  });

  it("links the signed-in header to the profile page", async () => {
    const account = {
      id: "1",
      email: "op@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
    };
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          new Response(JSON.stringify(account), { status: 200 }),
        ),
    );

    render(<App />);

    const profileLink = await screen.findByRole("link", { name: /profile/i });
    expect(profileLink).toHaveAttribute("href", "/profile");
  });

  it("links the signed-in header to My Nets", async () => {
    const account = {
      id: "1",
      email: "op@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
    };
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          new Response(JSON.stringify(account), { status: 200 }),
        ),
    );

    render(<App />);

    const myNetsLink = await screen.findByRole("link", { name: /my nets/i });
    expect(myNetsLink).toHaveAttribute("href", "/my-nets");
  });

  it("offers the admin entry to an admin account", async () => {
    // The dashboard is unreachable without a way in, and `isAdmin` from /me is
    // the only signal the shell has.
    const account = {
      id: "1",
      email: "admin@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
      isAdmin: true,
    };
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          new Response(JSON.stringify(account), { status: 200 }),
        ),
    );

    render(<App />);

    const adminLink = await screen.findByRole("link", { name: /^admin$/i });
    expect(adminLink).toHaveAttribute("href", "/admin");
  });

  it("hides the admin entry from a signed-in non-admin", async () => {
    // Advertising a surface the server would 403 is a dead end, not a hint.
    const account = {
      id: "1",
      email: "op@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
      isAdmin: false,
    };
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          new Response(JSON.stringify(account), { status: 200 }),
        ),
    );

    render(<App />);

    await screen.findByRole("link", { name: /my nets/i });
    expect(
      screen.queryByRole("link", { name: /^admin$/i }),
    ).not.toBeInTheDocument();
  });

  it("shows no admin entry when signed out", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          new Response(
            JSON.stringify({ type: "/errors/unauthenticated", status: 401 }),
            { status: 401 },
          ),
        ),
    );

    render(<App />);

    await screen.findByRole("link", { name: /sign in/i });
    expect(
      screen.queryByRole("link", { name: /^admin$/i }),
    ).not.toBeInTheDocument();
  });

  it("shows no My Nets link when signed out", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          new Response(
            JSON.stringify({ type: "/errors/unauthenticated", status: 401 }),
            { status: 401 },
          ),
        ),
    );

    render(<App />);

    // Once the session check resolves signed-out, the Sign in link appears and
    // My Nets does not.
    await screen.findByRole("link", { name: /sign in/i });
    expect(screen.queryByRole("link", { name: /my nets/i })).not.toBeInTheDocument();
  });

  it("shows the callsign beside the email once one is set", async () => {
    const account = {
      id: "1",
      email: "op@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
      callsign: "W1AW",
    };
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          new Response(JSON.stringify(account), { status: 200 }),
        ),
    );

    render(<App />);

    expect(await screen.findByText("W1AW")).toBeInTheDocument();
  });

  it("shows the signed-in account from /api/accounts/me and signs out", async () => {
    const user = userEvent.setup();
    const account = {
      id: "1",
      email: "op@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(JSON.stringify(account), { status: 200 }),
      )
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<App />);

    expect(await screen.findByText(account.email)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /sign out/i }));

    // Revocation goes to the API and the shell returns to signed-out.
    const [url, init] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(url).toBe("/api/sessions/current");
    expect(init.method).toBe("DELETE");
    const signIn = await screen.findByRole("link", { name: /sign in/i });
    expect(signIn).toHaveAttribute("href", "/sign-in");
    expect(screen.queryByText(account.email)).not.toBeInTheDocument();
  });
});
