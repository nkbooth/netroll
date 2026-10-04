// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { RouteObject } from "react-router";

import App from "./App";
import { ReportAbusePage } from "./features/abuse/ReportAbusePage";
import { AdminPage } from "./features/admin/AdminPage";
import { ConfirmEmailChangePage } from "./features/auth/ConfirmEmailChangePage";
import { DiscoveryPage } from "./features/discovery/DiscoveryPage";
import { SignInPage } from "./features/auth/SignInPage";
import { VerifyPage } from "./features/auth/VerifyPage";
import { ConsentPage } from "./features/consent/ConsentPage";
import { MyNetsPage } from "./features/nets/MyNetsPage";
import { NetDefinitionFormPage } from "./features/nets/NetDefinitionFormPage";
import { PublicNetPage } from "./features/nets/PublicNetPage";
import { ProfilePage } from "./features/profile/ProfilePage";
import { LiveSessionPage } from "./features/session/LiveSessionPage";
import { PublicLiveSessionPage } from "./features/session/PublicLiveSessionPage";
import { NotFoundPage, RouteError } from "./ui/NotFoundPage";

// Every page nests under the App shell (header/nav/theme-toggle/footer,
// as a child of the single "/" layout route — the shell persists
// across navigation instead of vanishing on every page but Discovery. Child
// paths are relative (no leading slash): react-router joins them onto the
// parent's "/".
const appChildRoutes: RouteObject[] = [
  {
    // Discovery is the public app root: the App shell wraps the
    // DiscoveryPage index child as its body. No auth gate — account-less
    // visitors see it.
    index: true,
    element: <DiscoveryPage />,
  },
  {
    path: "sign-in",
    element: <SignInPage />,
  },
  {
    // The emailed magic link lands here; consumption is click-driven.
    path: "auth/verify",
    element: <VerifyPage />,
  },
  {
    // The emailed email-change confirmation link lands here; single-use,
    // click-driven consumption (scanner-safe), then sign-in (all sessions
    // are revoked on a completed change).
    path: "auth/confirm-email-change",
    element: <ConfirmEmailChangePage />,
  },
  {
    // First-login consent gate; server-enforced, this page is its UX.
    path: "consent",
    element: <ConsentPage />,
  },
  {
    // Callsign set/change; this same page carries the other profile sections.
    path: "profile",
    element: <ProfilePage />,
  },
  {
    // Create a net definition; gated on consent + callsign.
    path: "nets/new",
    element: <NetDefinitionFormPage />,
  },
  {
    // Edit an owned net definition; prefilled by an owner-scoped load.
    path: "nets/:id/edit",
    element: <NetDefinitionFormPage />,
  },
  {
    // PUBLIC net view by link token; `t` is a static first
    // segment, so it never collides with `/nets/:id/edit` or `/nets/new`.
    path: "nets/t/:token",
    element: <PublicNetPage />,
  },
  {
    // "My Nets" — the account's favorited nets. Self-gated:
    // signed-out visitors are redirected to /sign-in by the page itself.
    path: "my-nets",
    element: <MyNetsPage />,
  },
  {
    // Owner-facing live-session page: streams over WebSocket and
    // self-recovers. Self-gated — signed-out visitors are redirected to
    // /sign-in by the page itself; the server is the real authority.
    path: "net-sessions/:id",
    element: <LiveSessionPage />,
  },
  {
    // PUBLIC, account-less live-session view by session id: a
    // shareable link renders the read-only roster + frequency over the redacted
    // public stream. `live` is a static first segment, so it never collides with
    // `/net-sessions/:id`. No auth gate — mirrors discovery's account-less root.
    path: "live/:id",
    element: <PublicLiveSessionPage />,
  },
  {
    // Public "Report abuse" affordance: a generic
    // support/contact report form. No auth gate — reachable from the footer on
    // any public surface.
    path: "report-abuse",
    element: <ReportAbusePage />,
  },
  {
    // Platform-admin dashboard (report queue, account lookup + disable, audit
    // log). Self-gated: signed-out visitors go to /sign-in and signed-in
    // non-admins to the root, by the page itself; the server's admin gate is
    // the real authority.
    path: "admin",
    element: <AdminPage />,
  },
  {
    // Catch-all: unmatched client paths get NetRoll's own 404, not
    // react-router's default error UI.
    path: "*",
    element: <NotFoundPage />,
  },
];

const appRoutes: RouteObject[] = [
  {
    path: "/",
    element: <App />,
    children: appChildRoutes,
  },
];

/** Stamps an `errorElement` onto a route and every descendant, recursively —
 * so a thrown loader/render error surfaces the app's own error page at
 * whichever level it occurred, instead of the framework default. */
function withErrorElements(route: RouteObject): RouteObject {
  // An index route's type forbids `children` entirely (it's a leaf by
  // definition) — omitting the key when there's nothing to map, rather than
  // setting it to `undefined`, matches that shape at runtime. TS can't
  // narrow the object-literal return through this generic recursive
  // signature's index/non-index union on its own, so the cast documents an
  // invariant this module itself guarantees: `appChildRoutes` never puts
  // `children` on its one `index: true` entry.
  return {
    ...route,
    errorElement: <RouteError />,
    ...(route.children
      ? { children: route.children.map(withErrorElements) }
      : {}),
  } as RouteObject;
}

/**
 * Route table for the NetRoll SPA. The browser router is created in
 * `main.tsx`; keeping this module DOM-free lets tests mount the routes on a
 * memory router.
 */
export const routes: RouteObject[] = appRoutes.map(withErrorElements);
