// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useState } from "react";
import type { CSSProperties, FormEvent, ReactElement } from "react";

import { tokens } from "../../ui/tokens/tokens";
import { submitAbuseReport } from "./abuseApi";
import {
  fetchFormToken,
  withBotMitigation,
} from "../botMitigation/botMitigation";
import { messageForProblem } from "../../errors/problemMessages";
import { useAuthRequest } from "../auth/useAuthRequest";

const pageStyle: CSSProperties = {
  maxWidth: "560px",
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

const labelStyle: CSSProperties = {
  display: "block",
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
  margin: "var(--space-3) 0 var(--space-1)",
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

const textareaStyle: CSSProperties = {
  ...inputStyle,
  minHeight: "8rem",
  resize: "vertical",
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

/** Off-screen honeypot, kept accessible with a "leave blank" name. */
const honeypotStyle: CSSProperties = {
  position: "absolute",
  left: "-9999px",
  width: "1px",
  height: "1px",
  overflow: "hidden",
};

/** Upper bound mirroring the server's `MAX_REPORT_BODY_LEN`. */
const MAX_BODY_LEN = 4_000;

/**
 * Public "Report abuse" form. Unauthenticated — the
 * affordance is on public surfaces. Reuses the sign-in bot-mitigation
 * participation (honeypot + form token). Records a report server-side; the
 * acknowledgement is deliberately neutral (no id, no confirmation oracle).
 */
export function ReportAbusePage(): ReactElement {
  const [body, setBody] = useState("");
  const [contact, setContact] = useState("");
  const [formToken, setFormToken] = useState<string | null | undefined>(
    undefined,
  );
  const [honeypot, setHoneypot] = useState("");
  useEffect(() => {
    void fetchFormToken().then(setFormToken);
  }, []);

  // The footer's "Report abuse" affordance is a plain
  // full-page navigation TO this page, so `window.location.href` read here
  // would always just be this page's own URL — never the page the reporter
  // was actually looking at. `document.referrer` is the browser's own record
  // of the page navigated FROM, so it correctly captures the offending page
  // for the common footer-link path. A direct visit (bookmark, typed URL, no
  // referrer sent) leaves it blank — omitted rather than sent as a useless
  // self-referential value.
  const contextUrl =
    typeof document !== "undefined" && document.referrer
      ? document.referrer
      : undefined;
  const { state, run } = useAuthRequest(() =>
    submitAbuseReport(
      {
        body: body.trim(),
        reporterContact: contact.trim() || undefined,
        contextUrl,
      },
      withBotMitigation({}, formToken ?? null, honeypot),
    ),
  );
  const formTokenLoading = formToken === undefined;

  function handleSubmit(event: FormEvent): void {
    event.preventDefault();
    void run();
  }

  if (state.status === "success") {
    return (
      <main style={pageStyle}>
        <h1 style={headingStyle}>Thanks for the report</h1>
        <p>
          We&apos;ve recorded it. An administrator will review it — you
          don&apos;t need to do anything else.
        </p>
      </main>
    );
  }

  return (
    <main style={pageStyle}>
      <h1 style={headingStyle}>Report abuse</h1>
      <p>
        Tell us what&apos;s wrong — spam, harassment, or anything that
        doesn&apos;t belong. You don&apos;t need an account to report.
      </p>
      <form onSubmit={handleSubmit}>
        <label htmlFor="report-body" style={labelStyle}>
          What happened
        </label>
        <textarea
          id="report-body"
          required
          maxLength={MAX_BODY_LEN}
          value={body}
          onChange={(event) => setBody(event.target.value)}
          disabled={state.status === "loading"}
          style={textareaStyle}
        />
        <label htmlFor="report-contact" style={labelStyle}>
          Your contact (optional)
        </label>
        <input
          id="report-contact"
          type="text"
          autoComplete="off"
          value={contact}
          onChange={(event) => setContact(event.target.value)}
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
          Send report
        </button>
      </form>
      {state.status === "error" && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(state.problem)}
        </p>
      )}
    </main>
  );
}
