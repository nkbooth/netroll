// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect } from "react";
import type { CSSProperties, ReactElement } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";

import { tokens } from "../../ui/tokens/tokens";
import { Card } from "../../ui/components/Card";
import { postSignInDestination } from "../consent/consentGate";
import { createSession } from "./authApi";
import { messageForProblem } from "../../errors/problemMessages";
import { useAuthRequest } from "./useAuthRequest";

const pageStyle: CSSProperties = {
  maxWidth: "440px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
};

const cardStyle: CSSProperties = {
  padding: "var(--space-6)",
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
 * Lands the emailed magic link. Consumption happens ONLY on the explicit
 * button click — email scanners prefetch links, and rendering must never
 * burn the single-use token.
 */
export function VerifyPage(): ReactElement {
  const [params] = useSearchParams();
  const token = params.get("token") ?? "";
  const navigate = useNavigate();
  const { state, run } = useAuthRequest(() => createSession(token));

  useEffect(() => {
    if (state.status === "success") {
      // Consent gates the landing when required; the gate carries the
      // intended destination and returns there after acceptance.
      void navigate(postSignInDestination(state.data.consentRequired), {
        replace: true,
        state: { returnTo: "/" },
      });
    }
  }, [state, navigate]);

  if (state.status === "error") {
    return (
      <main style={pageStyle}>
        <Card style={cardStyle}>
          <h1 style={headingStyle}>Couldn't sign you in</h1>
          <p role="alert" style={errorStyle}>
            {messageForProblem(state.problem)}
          </p>
          <p>
            <Link to="/sign-in">Request a new link</Link>
          </p>
        </Card>
      </main>
    );
  }

  return (
    <main style={pageStyle}>
      <Card style={cardStyle}>
        <h1 style={headingStyle}>Almost there</h1>
        <p>You followed a NetRoll sign-in link. Finish signing in below.</p>
        <button
          type="button"
          onClick={() => void run()}
          disabled={state.status === "loading"}
          style={primaryButtonStyle}
        >
          Sign in
        </button>
      </Card>
    </main>
  );
}
