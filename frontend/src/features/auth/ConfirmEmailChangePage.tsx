// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";
import { Link, useSearchParams } from "react-router";

import { tokens } from "../../ui/tokens/tokens";
import { confirmEmailChange } from "../profile/emailChangeApi";
import { messageForProblem } from "../../errors/problemMessages";
import { useAuthRequest } from "./useAuthRequest";

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

const primaryButtonStyle: CSSProperties = {
  marginTop: "var(--space-4)",
  padding: "var(--space-2) var(--space-4)",
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
  marginTop: "var(--space-3)",
};

/**
 * Lands the emailed email-change confirmation link. Consumption happens
 * ONLY on the explicit button click — email scanners prefetch links, and
 * rendering must never burn the single-use token. On success every session
 * is revoked, so the only path forward is a fresh sign-in with the
 * new address.
 */
export function ConfirmEmailChangePage(): ReactElement {
  const [params] = useSearchParams();
  const token = params.get("token") ?? "";
  const { state, run } = useAuthRequest(() => confirmEmailChange(token));

  if (state.status === "success") {
    return (
      <main style={pageStyle}>
        <h1 style={headingStyle}>Email confirmed</h1>
        <p>
          Your email is now {state.data.email}. Sign in again with your new
          address.
        </p>
        <p>
          <Link to="/sign-in">Go to sign-in</Link>
        </p>
      </main>
    );
  }

  if (state.status === "error") {
    return (
      <main style={pageStyle}>
        <h1 style={headingStyle}>Couldn&rsquo;t confirm your new email</h1>
        <p role="alert" style={errorStyle}>
          {messageForProblem(state.problem)}
        </p>
        <p>
          <Link to="/profile">Request a new link from your profile</Link>
        </p>
      </main>
    );
  }

  return (
    <main style={pageStyle}>
      <h1 style={headingStyle}>Confirm your new email</h1>
      <p>
        You followed a NetRoll email-change link. Confirm it below to finish
        moving your account.
      </p>
      <button
        type="button"
        onClick={() => void run()}
        disabled={state.status === "loading"}
        style={primaryButtonStyle}
      >
        Confirm new email
      </button>
    </main>
  );
}
