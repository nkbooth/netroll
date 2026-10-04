// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useState } from "react";
import type { CSSProperties, FormEvent, ReactElement } from "react";

import { tokens } from "../../ui/tokens/tokens";
import { Card } from "../../ui/components/Card";
import { Logomark } from "../../ui/components/Logomark";
import { requestMagicLink } from "./authApi";
import {
  fetchFormToken,
  withBotMitigation,
} from "../botMitigation/botMitigation";
import { messageForProblem } from "../../errors/problemMessages";
import { useAuthRequest } from "./useAuthRequest";

const pageStyle: CSSProperties = {
  maxWidth: "440px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
};

const cardStyle: CSSProperties = {
  padding: "var(--space-6)",
  textAlign: "center",
  fontSize: tokens.typography.body.fontSize,
  lineHeight: tokens.typography.body.lineHeight,
};

const headingStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  margin: "0 0 var(--space-4)",
};

const labelStyle: CSSProperties = {
  display: "block",
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
  textAlign: "left",
  marginBottom: "var(--space-1)",
};

const formStyle: CSSProperties = {
  textAlign: "left",
};

const inputStyle: CSSProperties = {
  display: "block",
  width: "100%",
  padding: "var(--space-2) var(--space-3)",
  background: "var(--surface)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
};

const primaryButtonStyle: CSSProperties = {
  marginTop: "var(--space-4)",
  width: "100%",
  padding: "var(--space-3) var(--space-4)",
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
 * Off-screen honeypot: kept out of the visual + tab flow but still in the
 * accessibility tree with a clear "leave blank" name, so a real user (or AT)
 * never fills it while a naive autofill bot does.
 */
const honeypotStyle: CSSProperties = {
  position: "absolute",
  left: "-9999px",
  width: "1px",
  height: "1px",
  overflow: "hidden",
};

/** The sign-in card's mark: the shared brand logomark, one step up from the
 * shell-chrome size. */
const CARD_LOGOMARK_SIZE = 44;

const checkCircleStyle: CSSProperties = {
  width: "46px",
  height: "46px",
  borderRadius: "var(--rounded-full)",
  background: "var(--live-fill)",
  border: "1px solid var(--live-border)",
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  marginBottom: "var(--space-4)",
};

const dividerRowStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  margin: "var(--space-5) 0",
};

const dividerLineStyle: CSSProperties = {
  flex: 1,
  height: "1px",
  background: "var(--border)",
};

const dividerLabelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  color: "var(--text-muted)",
};

const googleButtonStyle: CSSProperties = {
  width: "100%",
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  color: "var(--text-muted)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-3) var(--space-4)",
  font: "inherit",
  fontWeight: 700,
  cursor: "not-allowed",
};

const footnoteStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  marginTop: "var(--space-5)",
  lineHeight: 1.5,
};

const tipBoxStyle: CSSProperties = {
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-lg)",
  padding: "var(--space-3) var(--space-4)",
  textAlign: "left",
  display: "flex",
  gap: "var(--space-3)",
  alignItems: "flex-start",
  marginTop: "var(--space-4)",
};

const tipTextStyle: CSSProperties = {
  fontSize: "12.5px",
  color: "var(--text-muted)",
  lineHeight: 1.5,
};

const inlineLinkButtonStyle: CSSProperties = {
  background: "none",
  border: "none",
  padding: 0,
  font: "inherit",
  fontWeight: 700,
  color: "var(--accent-ink)",
  cursor: "pointer",
  textDecoration: "underline",
};

const backLinkStyle: CSSProperties = {
  marginTop: "var(--space-4)",
  background: "transparent",
  border: "none",
  color: "var(--text-muted)",
  fontWeight: 700,
  fontSize: tokens.typography.labelCaps.fontSize,
  font: "inherit",
  cursor: "pointer",
};

/** Checkmark-in-circle badge marking the link-sent success state. */
function CheckCircleBadge(): ReactElement {
  return (
    <span aria-hidden="true" style={checkCircleStyle}>
      <svg
        viewBox="0 0 24 24"
        width="22"
        height="22"
        fill="none"
        stroke="var(--live-text)"
        strokeWidth={2.4}
      >
        <path d="M20 6 9 17l-5-5" />
      </svg>
    </span>
  );
}

/** Info glyph for the "no email yet?" tip callout. */
function InfoIcon(): ReactElement {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      width="16"
      height="16"
      fill="none"
      stroke="var(--accent-ink)"
      strokeWidth={2.2}
      style={{ flex: "0 0 auto", marginTop: "1px" }}
    >
      <circle cx="12" cy="12" r="9" />
      <path d="M12 8v5M12 16h.01" />
    </svg>
  );
}

/**
 * Email-first sign-in: request a single-use magic link. Registration and
 * sign-in are the same flow, so this page never distinguishes them.
 */
export function SignInPage(): ReactElement {
  const [email, setEmail] = useState("");
  // Bot-mitigation state: a form token fetched on mount and an
  // always-empty honeypot a real user never fills. `undefined` distinctly
  // means "the mount fetch hasn't resolved yet" (gates submit below), separate
  // from a resolved `null` (mitigation disabled on this instance) — a fast
  // submit racing ahead of the mount fetch would otherwise read as a missing
  // token, which the server treats as a bot.
  const [formToken, setFormToken] = useState<string | null | undefined>(
    undefined,
  );
  const [honeypot, setHoneypot] = useState("");
  useEffect(() => {
    void fetchFormToken().then(setFormToken);
  }, []);
  // Mirrors the backend's normalization: what we request, confirm, and mail
  // is one and the same mailbox, whatever casing was typed.
  const normalizedEmail = email.trim().toLowerCase();
  const { state, run, reset } = useAuthRequest(() =>
    requestMagicLink(
      normalizedEmail,
      withBotMitigation({}, formToken ?? null, honeypot),
    ),
  );
  const formTokenLoading = formToken === undefined;

  function handleSubmit(event: FormEvent): void {
    event.preventDefault();
    void run();
  }

  /** Back to an editable form for a fresh address — the sent state retires. */
  function handleUseDifferentEmail(): void {
    reset();
    setEmail("");
  }

  if (state.status === "success") {
    return (
      <main style={pageStyle}>
        <Card style={cardStyle}>
          <CheckCircleBadge />
          <h1 style={headingStyle}>Check your email</h1>
          <p>
            We sent a sign-in link to <strong>{normalizedEmail}</strong>. It's
            good for 15 minutes and works once.
          </p>
          <div style={tipBoxStyle}>
            <InfoIcon />
            <span style={tipTextStyle}>
              No email in a minute or two? Check spam, or{" "}
              <button
                type="button"
                onClick={() => void run()}
                style={inlineLinkButtonStyle}
              >
                send it again
              </button>
              . Keep this tab open — the link brings you right back to where
              you were.
            </span>
          </div>
          <button
            type="button"
            onClick={handleUseDifferentEmail}
            style={backLinkStyle}
          >
            ← Use a different email
          </button>
        </Card>
      </main>
    );
  }

  return (
    <main style={pageStyle}>
      <Card style={cardStyle}>
        <Logomark
          size={CARD_LOGOMARK_SIZE}
          style={{ marginBottom: "var(--space-4)" }}
        />
        <h1 style={headingStyle}>Sign in to NetRoll</h1>
        <p>
          No password — we email you a single-use sign-in link. New here? The
          same link creates your account.
        </p>
        <form onSubmit={handleSubmit} style={formStyle}>
          <label htmlFor="sign-in-email" style={labelStyle}>
            Email
          </label>
          <input
            id="sign-in-email"
            type="email"
            autoComplete="email"
            required
            value={email}
            onChange={(event) => setEmail(event.target.value)}
            disabled={state.status === "loading"}
            style={inputStyle}
          />
          <input
            type="text"
            name="hp_field"
            aria-label="Leave this field blank"
            tabIndex={-1}
            autoComplete="off"
            value={honeypot}
            onChange={(event) => setHoneypot(event.target.value)}
            style={honeypotStyle}
          />
          <button
            type="submit"
            disabled={state.status === "loading" || formTokenLoading}
            style={primaryButtonStyle}
          >
            Email me a sign-in link
          </button>
        </form>
        {state.status === "error" && (
          <p role="alert" style={errorStyle}>
            {messageForProblem(state.problem)}
          </p>
        )}
        <div style={dividerRowStyle}>
          <span style={dividerLineStyle} />
          <span style={dividerLabelStyle}>SOON</span>
          <span style={dividerLineStyle} />
        </div>
        <button type="button" disabled style={googleButtonStyle}>
          Continue with Google
        </button>
        <p style={footnoteStyle}>
          By continuing you agree to the Terms and Privacy Notice.
        </p>
      </Card>
    </main>
  );
}
