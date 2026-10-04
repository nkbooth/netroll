// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useLocation, useNavigate } from "react-router";

import { tokens } from "../../ui/tokens/tokens";
import { Card } from "../../ui/components/Card";
import {
  ProblemError,
  deleteCurrentSession,
  fetchCurrentAccount,
} from "../auth/authApi";
import type { Account, Problem } from "../auth/authApi";
import { messageForProblem } from "../../errors/problemMessages";
import { useAuthRequest } from "../auth/useAuthRequest";
import { recordConsent } from "./consentApi";
import { gateDecision } from "./consentGate";

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

const eyebrowStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--accent-ink)",
};

const headingStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  margin: "var(--space-1) 0 var(--space-4)",
};

const checkboxRowStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-3)",
  alignItems: "flex-start",
  padding: "var(--space-3)",
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  marginBottom: "var(--space-3)",
  cursor: "pointer",
};

const checkboxInputStyle: CSSProperties = {
  width: "18px",
  height: "18px",
  marginTop: "1px",
  flex: "0 0 auto",
  accentColor: "var(--accent-deep)",
  cursor: "pointer",
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

const ghostButtonStyle: CSSProperties = {
  marginTop: "var(--space-4)",
  padding: "var(--space-2) var(--space-4)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
  marginTop: "var(--space-3)",
};

const buttonRowStyle: CSSProperties = {
  display: "flex",
  flexWrap: "wrap",
  gap: "var(--space-3)",
};

const footnoteStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  textAlign: "center",
  marginTop: "var(--space-4)",
};

/**
 * First-login consent gate: shows the versioned terms and
 * records acceptance before the user proceeds. The server-side guard is the
 * control — this page is the UX for it. Declining is signing out; there is
 * no skip into the app.
 */
export function ConsentPage(): ReactElement {
  const navigate = useNavigate();
  const location = useLocation();
  // Gate-and-return: the intended destination rides along and is restored
  // after acceptance.
  const returnTo =
    (location.state as { returnTo?: string } | null)?.returnTo ?? "/";

  // undefined = still loading /me; null = signed out (401 only — a thrown
  // ProblemError is a server/network failure, never "signed out").
  const [account, setAccount] = useState<Account | null | undefined>(undefined);
  // `null` = the account check has not failed; `undefined` = it failed with no
  // problem body; a `Problem` = the server said why. This was once a
  // bare boolean and the `catch` below discarded the problem entirely, so the
  // reason the server gave could never reach the alert.
  const [loadProblem, setLoadProblem] = useState<Problem | null | undefined>(
    null,
  );
  // Tracks an in-flight /me refetch independent of useAuthRequest's own
  // status: reset() (below) returns the consent request to idle the instant
  // a version-mismatch lands, but the refetch it kicks off to learn the new
  // requiredVersion is still pending. A fast re-click of "I agree" in that
  // window would resubmit the now-stale closed-over version (the item-#8
  // race) — gate the buttons on this flag too, not just request status.
  const [accountRefreshing, setAccountRefreshing] = useState(false);
  // Both boxes are required before "Agree & continue" is enabled — the
  // server enforces terms acceptance, but visibility consent is UX-only, so
  // this gate is the only place it's checked.
  const [agreedToTerms, setAgreedToTerms] = useState(false);
  const [agreedToVisibility, setAgreedToVisibility] = useState(false);
  const loadAccount = useCallback(async () => {
    setAccountRefreshing(true);
    try {
      const result = await fetchCurrentAccount();
      setLoadProblem(null);
      setAccount(result);
    } catch (error: unknown) {
      setLoadProblem(error instanceof ProblemError ? error.problem : undefined);
    } finally {
      setAccountRefreshing(false);
    }
  }, []);
  useEffect(() => {
    void loadAccount();
  }, [loadAccount]);

  const requiredVersion = account?.requiredTermsVersion ?? "";
  const { state, run, reset } = useAuthRequest(() =>
    recordConsent(requiredVersion),
  );

  // Route away when there is nothing to gate.
  useEffect(() => {
    if (account === undefined) {
      return;
    }
    switch (gateDecision(account)) {
      case "signed-out":
        void navigate("/sign-in", { replace: true });
        break;
      case "through":
        void navigate(returnTo, { replace: true });
        break;
      case "gate":
        break;
    }
  }, [account, navigate, returnTo]);

  useEffect(() => {
    if (state.status === "success") {
      void navigate(returnTo, { replace: true });
    }
    if (
      state.status === "error" &&
      state.problem?.type === "/errors/consent-version-mismatch"
    ) {
      // A stale gate must not dead-end: re-read /me to learn the version
      // the server now requires, and re-arm the request.
      void loadAccount();
      reset();
    }
    if (
      state.status === "error" &&
      state.problem?.type === "/errors/unauthenticated"
    ) {
      // The session expired while the gate was open — there is nothing to
      // retry here, only sign-in again.
      void navigate("/sign-in", { replace: true });
    }
  }, [state, navigate, returnTo, loadAccount, reset]);

  async function signOut(): Promise<void> {
    try {
      await deleteCurrentSession();
    } catch {
      // Leaving the gate is still right on failure: the shell re-reads /me
      // and reflects whatever the server says.
    }
    void navigate("/", { replace: true });
  }

  if (loadProblem !== null) {
    return (
      <main style={pageStyle}>
        <Card style={cardStyle}>
          <p role="alert" style={errorStyle}>
            {messageForProblem(loadProblem)}
          </p>
          <button
            type="button"
            onClick={() => void loadAccount()}
            style={primaryButtonStyle}
          >
            Try again
          </button>
        </Card>
      </main>
    );
  }

  if (account === undefined || account === null || !account.consentRequired) {
    // Redirect effects are in flight; render nothing rather than a flash.
    return <main style={pageStyle} />;
  }

  const canContinue =
    agreedToTerms &&
    agreedToVisibility &&
    state.status !== "loading" &&
    !accountRefreshing;

  return (
    <main style={pageStyle}>
      <Card style={cardStyle}>
        <div style={eyebrowStyle}>First login · one quick step</div>
        <h1 style={headingStyle}>Before you check in</h1>
        <label style={checkboxRowStyle}>
          <input
            type="checkbox"
            checked={agreedToTerms}
            onChange={(event) => setAgreedToTerms(event.target.checked)}
            style={checkboxInputStyle}
          />
          <span>
            I agree to the Terms of Service and Privacy Notice.
          </span>
        </label>
        <label style={checkboxRowStyle}>
          <input
            type="checkbox"
            checked={agreedToVisibility}
            onChange={(event) => setAgreedToVisibility(event.target.checked)}
            style={checkboxInputStyle}
          />
          <span>
            I understand my callsign and check-ins are{" "}
            <strong>publicly visible</strong> during nets.
          </span>
        </label>
        <div style={buttonRowStyle}>
          <button
            type="button"
            onClick={() => void run()}
            disabled={!canContinue}
            style={primaryButtonStyle}
          >
            Agree &amp; continue
          </button>
          <button
            type="button"
            onClick={() => void signOut()}
            disabled={state.status === "loading" || accountRefreshing}
            style={ghostButtonStyle}
          >
            Sign out
          </button>
        </div>
        {state.status === "error" && (
          <p role="alert" style={errorStyle}>
            {messageForProblem(state.problem)}
          </p>
        )}
        <p style={footnoteStyle}>Next: choose your callsign.</p>
      </Card>
    </main>
  );
}
