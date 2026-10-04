// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ReportAbusePage } from "./ReportAbusePage";
import { messageForProblemType } from "../../errors/problemMessages";
import { expectNoAxeViolations } from "../../test/axe";

function problemResponse(
  status: number,
  type: string,
  detail?: string,
): Response {
  return new Response(JSON.stringify({ type, status, detail }), {
    status,
    headers: { "content-type": "application/problem+json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ReportAbusePage", () => {
  it("has a labeled report field and no WCAG 2.1 AA violations", async () => {
    vi.stubGlobal("fetch", vi.fn());
    const { container } = render(<ReportAbusePage />);
    expect(screen.getByLabelText(/what happened/i)).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it("submits the entered body plus the (empty) honeypot and swaps to a thank-you", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<ReportAbusePage />);
    await user.type(screen.getByLabelText(/what happened/i), "someone is spamming");
    await user.click(screen.getByRole("button", { name: /send report/i }));

    // Retires the form for a neutral acknowledgement (no id echoed back).
    expect(await screen.findByRole("heading", { name: /thanks/i })).toBeInTheDocument();
    expect(screen.queryByLabelText(/what happened/i)).not.toBeInTheDocument();

    const call = fetchMock.mock.calls.find(([u]) => u === "/api/abuse-reports") as [
      string,
      RequestInit,
    ];
    expect(call).toBeDefined();
    const payload = JSON.parse(call[1].body as string);
    expect(payload.body).toBe("someone is spamming");
    // The honeypot participates and is empty for a human.
    expect(payload.hpField).toBe("");
  });

  it("sends document.referrer as contextUrl (the page the reporter came from), not this page's own URL", async () => {
    // The footer link fully navigates TO this page, so
    // `window.location.href` would always just be `/report-abuse` itself —
    // useless to an admin investigating a report. `document.referrer` is the
    // page navigated FROM and is what must be sent.
    Object.defineProperty(document, "referrer", {
      value: "https://example.com/nets/w1aw-weekly",
      configurable: true,
    });
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<ReportAbusePage />);
    await user.type(screen.getByLabelText(/what happened/i), "abusive net title");
    await user.click(screen.getByRole("button", { name: /send report/i }));
    await screen.findByRole("heading", { name: /thanks/i });

    const call = fetchMock.mock.calls.find(([u]) => u === "/api/abuse-reports") as [
      string,
      RequestInit,
    ];
    const payload = JSON.parse(call[1].body as string);
    expect(payload.contextUrl).toBe("https://example.com/nets/w1aw-weekly");

    Object.defineProperty(document, "referrer", { value: "", configurable: true });
  });

  it("omits contextUrl on a direct visit with no referrer", async () => {
    Object.defineProperty(document, "referrer", { value: "", configurable: true });
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<ReportAbusePage />);
    await user.type(screen.getByLabelText(/what happened/i), "abusive net title");
    await user.click(screen.getByRole("button", { name: /send report/i }));
    await screen.findByRole("heading", { name: /thanks/i });

    const call = fetchMock.mock.calls.find(([u]) => u === "/api/abuse-reports") as [
      string,
      RequestInit,
    ];
    const payload = JSON.parse(call[1].body as string);
    expect(payload.contextUrl).toBeUndefined();
  });

  it("maps a rate-limit problem to its shared friendly message, not raw copy", async () => {
    const user = userEvent.setup();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(problemResponse(429, "/errors/rate-limited")),
    );

    render(<ReportAbusePage />);
    await user.type(screen.getByLabelText(/what happened/i), "spam");
    await user.click(screen.getByRole("button", { name: /send report/i }));

    const alert = await screen.findByRole("alert");
    // Assert the DECISION (the mapped message from the shared harness), never a
    // literal string duplicated in the test.
    await waitFor(() =>
      expect(alert).toHaveTextContent(
        messageForProblemType("/errors/rate-limited"),
      ),
    );
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          problemResponse(429, "/errors/rate-limited", detail),
        ),
    );

    render(<ReportAbusePage />);
    await user.type(screen.getByLabelText(/what happened/i), "spam");
    await user.click(screen.getByRole("button", { name: /send report/i }));

    const alert = await screen.findByRole("alert");
    await waitFor(() => expect(alert).toHaveTextContent(detail));
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/rate-limited"),
    );
  });

});
