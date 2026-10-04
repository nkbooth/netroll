// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";
import { Link } from "react-router";

import { tokens } from "./tokens/tokens";

/**
 * App-defined error surfaces for the SPA: a 404 for unmatched client paths
 * (the catch-all route) and a route-error boundary (`errorElement`), so an
 * unknown URL or a thrown route error lands on NetRoll's own keyboard-operable
 * page with a way home — never react-router's bare default UI.
 */

const pageStyle: CSSProperties = {
  maxWidth: "420px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
  fontSize: tokens.typography.body.fontSize,
  lineHeight: tokens.typography.body.lineHeight,
};

const headingStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  margin: "0 0 var(--space-4)",
};

const linkStyle: CSSProperties = {
  color: "var(--accent-ink)",
};

/** Catch-all 404 for unmatched client routes. */
export function NotFoundPage(): ReactElement {
  return (
    <main style={pageStyle}>
      <h1 style={headingStyle}>Page not found</h1>
      <p>That page doesn&rsquo;t exist here.</p>
      <p>
        <Link to="/" style={linkStyle}>
          Back to NetRoll
        </Link>
      </p>
    </main>
  );
}

/** Route-error boundary surface for a thrown loader/render error. */
export function RouteError(): ReactElement {
  return (
    <main style={pageStyle}>
      <h1 style={headingStyle}>Something&rsquo;s off on our end</h1>
      <p>This page hit an unexpected error.</p>
      <p>
        <Link to="/" style={linkStyle}>
          Back to NetRoll
        </Link>
      </p>
    </main>
  );
}
