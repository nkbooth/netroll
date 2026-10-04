// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";
import { Outlet } from "react-router";

import { useAppConfig } from "./features/appConfig/useAppConfig";
import { useCurrentAccount } from "./features/auth/useCurrentAccount";
import { LiveRegion } from "./ui/a11y/LiveRegion";
import { Logomark } from "./ui/components/Logomark";
import { ThemeToggle } from "./ui/components/ThemeToggle";
import { tokens } from "./ui/tokens/tokens";

/** Max content measure from DESIGN.md § Layout & Spacing. Chrome bars run
 * full-bleed; their interiors share this measure with every page body. */
const CONTENT_MAX_WIDTH = "1200px";

const contentWidthStyle: CSSProperties = {
  maxWidth: CONTENT_MAX_WIDTH,
  margin: "0 auto",
  width: "100%",
};

const headerStyle: CSSProperties = {
  padding: "var(--space-3) var(--space-page-x)",
  borderBottom: "1px solid var(--border)",
  background: "var(--head-grad)",
};

const headerRowStyle: CSSProperties = {
  ...contentWidthStyle,
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  gap: "var(--space-4)",
};

const wordmarkStyle: CSSProperties = {
  margin: 0,
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  fontSize: tokens.typography.wordmark.fontSize,
  fontWeight: tokens.typography.wordmark.fontWeight,
  letterSpacing: tokens.typography.wordmark.letterSpacing,
};

const wordmarkLinkStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  color: "var(--text)",
  textDecoration: "none",
};

// The callsign is attribution, not the mark (DESIGN.md § Brand & Style) — it
// sits in cyan ink beside the wordmark rather than competing with it in weight.
const bylineStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
  color: "var(--accent-ink)",
};

const authAreaStyle: CSSProperties = {
  display: "flex",
  alignItems: "baseline",
  gap: "var(--space-3)",
  fontSize: tokens.typography.body.fontSize,
};

// Email is identity, not radio data — sans, never mono.
const accountEmailStyle: CSSProperties = {
  color: "var(--text-muted)",
};

// Callsigns are radio data — the one heritage mono nod (DESIGN.md).
const callsignStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  fontWeight: tokens.typography.callsign.fontWeight,
};

const profileLinkStyle: CSSProperties = {
  color: "var(--accent-ink)",
};

const signOutStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-3)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const signInLinkStyle: CSSProperties = {
  color: "var(--accent-ink)",
};

const footerStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-page-x)",
  borderTop: "1px solid var(--border)",
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
};

const footerRowStyle: CSSProperties = {
  ...contentWidthStyle,
  display: "flex",
  justifyContent: "center",
  gap: "var(--space-4)",
};

const footerLinkStyle: CSSProperties = {
  color: "var(--text-muted)",
};

const signOutFailedStyle: CSSProperties = {
  color: "var(--warn)",
  fontSize: tokens.typography.meta.fontSize,
};

/**
 * NetRoll app shell: full-bleed header and footer bars whose interiors share
 * the page content measure, carrying the logomark + wordmark (linked back to
 * the discovery root), the signed-in indicator, and the theme switch.
 */
export default function App(): ReactElement {
  const { account, loading, signOutFailed, signOut } = useCurrentAccount();
  // Operator-opt-in integrations; everything is off until configured, and a
  // failed read stays off (see `useAppConfig`).
  const { kofiUsername } = useAppConfig();

  return (
    <>
      <header style={headerStyle}>
        <div style={headerRowStyle}>
          <h1 style={wordmarkStyle}>
            <a href="/" style={wordmarkLinkStyle}>
              <Logomark />
              NetRoll
              <span style={bylineStyle}>by N1CCK</span>
            </a>
          </h1>
          <div style={authAreaStyle}>
          {account && (
            <>
              {signOutFailed && (
                <span role="alert" style={signOutFailedStyle}>
                  Sign out didn&apos;t go through — try again.
                </span>
              )}
              <span style={accountEmailStyle}>{account.email}</span>
              {account.callsign && (
                <span style={callsignStyle}>{account.callsign}</span>
              )}
              <a href="/my-nets" style={profileLinkStyle}>
                My Nets
              </a>
              <a href="/profile" style={profileLinkStyle}>
                Profile
              </a>
              {/* Platform-admin dashboard, offered only to an account the boot
                  ADMIN_ACCOUNT_EMAILS allowlist names. A render hint only —
                  every admin endpoint is server-gated regardless. */}
              {account.isAdmin && (
                <a href="/admin" style={profileLinkStyle}>
                  Admin
                </a>
              )}
              <button
                type="button"
                onClick={() => void signOut()}
                style={signOutStyle}
              >
                Sign out
              </button>
            </>
          )}
            {!account && !loading && (
              <a href="/sign-in" style={signInLinkStyle}>
                Sign in
              </a>
            )}
            <ThemeToggle />
          </div>
        </div>
      </header>
      <main>
        <Outlet />
      </main>
      <footer style={footerStyle}>
        <div style={footerRowStyle}>
          {/* Documentation, served by the backend out of the SPA bundle dir at
              /docs (see static.rs + the Containerfile `docs` stage). A plain
              href, deliberately: React Router would treat it as a client route,
              miss it, and render the not-found page. Auth-independent — an
              operator locked out mid-net still needs it. */}
          <a href="/docs/" style={footerLinkStyle}>
            Docs
          </a>
          {/* Public "Report abuse" affordance. */}
          <a href="/report-abuse" style={footerLinkStyle}>
            Report abuse
          </a>
          {/* Operator-opt-in support link (absent unless KOFI_USERNAME is set).
              A plain link, not Ko-fi's overlay widget: DESIGN.md's Do/Don't
              rules out donation pleas in the chrome, and a hosted widget would
              load third-party script on every page. */}
          {kofiUsername !== null && (
            <a
              href={`https://ko-fi.com/${kofiUsername}`}
              target="_blank"
              rel="noopener noreferrer"
              style={footerLinkStyle}
            >
              Support NetRoll
            </a>
          )}
        </div>
      </footer>
      <LiveRegion />
    </>
  );
}
