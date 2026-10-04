// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { routes } from "./router";
import { RouteError } from "./ui/NotFoundPage";

describe("router", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("renders the DiscoveryPage as the root body within the App shell", async () => {
    // The discovery landing is the app root: the App header chrome
    // stays, discovery renders as its body — public, no auth gate.
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation((url: unknown) => {
        const href = String(url);
        if (href.startsWith("/api/discovery")) {
          return Promise.resolve(
            new Response(
              JSON.stringify({
                activeNow: [],
                upcoming: [],
                applied: { sort: "time" },
              }),
              {
                status: 200,
                headers: { "content-type": "application/json" },
              },
            ),
          );
        }
        // /api/accounts/me → signed out.
        return Promise.resolve(
          new Response(
            JSON.stringify({ type: "/errors/unauthenticated", status: 401 }),
            { status: 401 },
          ),
        );
      }),
    );

    const router = createMemoryRouter(routes, { initialEntries: ["/"] });
    render(<RouterProvider router={router} />);

    expect(
      screen.getByRole("heading", { level: 1, name: /netroll/i }),
    ).toBeInTheDocument();
    expect(
      await screen.findByText(/no nets are live right now/i),
    ).toBeInTheDocument();
  });

  it("matches the root path to the App layout with the DiscoveryPage index child", () => {
    const router = createMemoryRouter(routes, { initialEntries: ["/"] });

    // The root now nests: the App shell layout wraps its DiscoveryPage index
    // child (discovery is the app root body).
    expect(router.state.matches).toHaveLength(2);
    expect(router.state.matches[0]?.route.path).toBe("/");
    expect(router.state.matches[1]?.route.index).toBe(true);
  });

  it("matches the auth routes added for magic-link sign-in, nested under the App shell", () => {
    for (const [path, childPath] of [
      ["/sign-in", "sign-in"],
      ["/auth/verify", "auth/verify"],
      ["/auth/confirm-email-change", "auth/confirm-email-change"],
      ["/profile", "profile"],
    ] as const) {
      const router = createMemoryRouter(routes, { initialEntries: [path] });

      // Every page nests under the "/" App-shell layout route so
      // the header/nav persists across navigation, not just on Discovery.
      expect(router.state.matches).toHaveLength(2);
      expect(router.state.matches[0]?.route.path).toBe("/");
      expect(router.state.matches[1]?.route.path).toBe(childPath);
    }
  });

  it("matches the /my-nets route to the MyNetsPage nested under the App shell", () => {
    const router = createMemoryRouter(routes, { initialEntries: ["/my-nets"] });

    expect(router.state.matches).toHaveLength(2);
    expect(router.state.matches[0]?.route.path).toBe("/");
    expect(router.state.matches[1]?.route.path).toBe("my-nets");
  });

  it("matches the /admin route to the AdminPage nested under the App shell", () => {
    const router = createMemoryRouter(routes, { initialEntries: ["/admin"] });

    expect(router.state.matches).toHaveLength(2);
    expect(router.state.matches[0]?.route.path).toBe("/");
    expect(router.state.matches[1]?.route.path).toBe("admin");
  });

  it("keeps the App shell header visible when navigating to a nested page", async () => {
    // The bug this guards: only Discovery used to nest under App, so the
    // header/nav vanished on every other page (e.g. My Nets).
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockImplementation(() =>
          Promise.resolve(
            new Response(
              JSON.stringify({ type: "/errors/unauthenticated", status: 401 }),
              { status: 401 },
            ),
          ),
        ),
    );

    const router = createMemoryRouter(routes, {
      initialEntries: ["/my-nets"],
    });
    render(<RouterProvider router={router} />);

    expect(
      await screen.findByRole("heading", { level: 1, name: /netroll/i }),
    ).toBeInTheDocument();
  });

  it("renders an app-defined NotFound surface for an unmatched path", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockImplementation(() =>
          Promise.resolve(
            new Response(
              JSON.stringify({ type: "/errors/unauthenticated", status: 401 }),
              { status: 401 },
            ),
          ),
        ),
    );

    const router = createMemoryRouter(routes, {
      initialEntries: ["/no-such-page"],
    });
    render(<RouterProvider router={router} />);

    // App-defined 404 (a real heading + a way home), not react-router's
    // default error UI. Now nested under the App shell, so its "go home"
    // link is one of several on screen (header nav, footer) — matched by
    // its accessible name, not positionally.
    expect(
      await screen.findByRole("heading", { name: /not found/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: /back to netroll/i }),
    ).toHaveAttribute("href", "/");
  });

  it("wires an errorElement on every route, including nested children, so a thrown error has an app surface", () => {
    const walk = (route: (typeof routes)[number]): void => {
      expect(route.errorElement).toBeDefined();
      route.children?.forEach(walk);
    };
    routes.forEach(walk);
  });

  it("renders the app error surface when a route throws", () => {
    const Boom = (): ReactElement => {
      throw new Error("route blew up");
    };
    const router = createMemoryRouter(
      [{ path: "/", element: <Boom />, errorElement: <RouteError /> }],
      { initialEntries: ["/"] },
    );
    render(<RouterProvider router={router} />);

    expect(screen.getByRole("heading")).toBeInTheDocument();
    expect(screen.getByRole("link")).toHaveAttribute("href", "/");
  });
});
