// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FrequencyControl } from "./FrequencyControl";
import { messageForProblemType } from "../../errors/problemMessages";
import { expectNoAxeViolations } from "../../test/axe";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}


/** One tunable HF way in, at `mhz` MHz. */
function ways(mhz: number) {
  return [
    {
      id: "00000000-0000-0000-0000-000000001604",
      position: 0,
      kind: "hf",
      plannedFrequencyHz: Math.round(mhz * 1_000_000),
      band: "20m",
      mode: "ssb",
      repeaterOffsetHz: null,
      toneMode: null,
      toneValue: null,
      node: null,
      reflector: null,
      network: null,
      talkgroup: null,
      label: null,
      detail: null,
    },
  ];
}

/** A blank wire connection, for building a way in of any kind. */
function blank(id: string, position: number, kind: string) {
  return {
    id,
    position,
    kind,
    plannedFrequencyHz: null as number | null,
    band: null as string | null,
    mode: null as string | null,
    repeaterOffsetHz: null,
    toneMode: null,
    toneValue: null,
    node: null as string | null,
    reflector: null,
    network: null,
    talkgroup: null,
    label: null,
    detail: null,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("FrequencyControl", () => {
  it("submits the entered frequency to changeFrequency for the session", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { id: "abc", lifecycle: "live" }));
    vi.stubGlobal("fetch", fetchMock);

    render(<FrequencyControl sessionId="abc" connections={ways(14.25)} />);
    const input = screen.getByLabelText(/operating frequency/i);
    await user.clear(input);
    await user.type(input, "7.200");
    await user.click(screen.getByRole("button", { name: /set frequency/i }));

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/frequency");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({
      connectionId: "00000000-0000-0000-0000-000000001604",
      operatingFrequency: "7.200",
    });
  });

  it("prefills the current folded frequency as the input value", () => {
    render(<FrequencyControl sessionId="abc" connections={ways(14.25)} />);
    expect(screen.getByLabelText(/operating frequency/i)).toHaveValue("14.25");
  });

  it("surfaces a mapped problem message on failure", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(409, { type: "/errors/session-already-closed", status: 409 }),
      );
    vi.stubGlobal("fetch", fetchMock);

    render(<FrequencyControl sessionId="abc" connections={ways(14.25)} />);
    await user.click(screen.getByRole("button", { name: /set frequency/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/session-already-closed"),
    );
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(
      <FrequencyControl sessionId="abc" connections={ways(14.25)} />,
    );
    await expectNoAxeViolations(container);
  });

  it("offers no way-in picker when only one way in can be retuned", () => {
    // A picker with one option is a control that cannot be used, and it reads as
    // a choice the operator does not have.
    render(<FrequencyControl sessionId="abc" connections={ways(14.25)} />);
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });

  it("retunes the way in the operator picked, not the first one", async () => {
    // The `<select>` appears only on a net with
    // more than one tunable way in, and until now nothing asserted that picking
    // the SECOND one actually addressed the second one — a control that always
    // submitted `tunable[0]` would have passed every other test in this file.
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { id: "abc", lifecycle: "live" }));
    vi.stubGlobal("fetch", fetchMock);

    const hf = {
      ...blank("00000000-0000-0000-0000-000000001604", 0, "hf"),
      plannedFrequencyHz: 14_250_000,
      band: "20m",
      mode: "ssb",
    };
    const repeater = {
      ...blank("00000000-0000-0000-0000-000000001605", 1, "repeater"),
      plannedFrequencyHz: 145_230_000,
      band: "2m",
      mode: "fm",
    };
    // An internet way in sits between them and must never be offered: it has no
    // frequency, and the server refuses a retune naming it.
    const echolink = {
      ...blank("00000000-0000-0000-0000-000000001606", 2, "echolink"),
      node: "12345",
    };

    render(
      <FrequencyControl
        sessionId="abc"
        connections={[hf, echolink, repeater]}
      />,
    );

    const picker = screen.getByRole("combobox");
    expect(
      screen.getAllByRole("option").map((o) => (o as HTMLOptionElement).value),
    ).toEqual([hf.id, repeater.id]);

    await user.selectOptions(picker, repeater.id);
    // Picking a way in re-prefills from THAT way in, not from the first.
    expect(screen.getByLabelText(/operating frequency/i)).toHaveValue("145.23");

    await user.click(screen.getByRole("button", { name: /set frequency/i }));
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string).connectionId).toBe(repeater.id);
  });

  it("renders nothing for an internet-only net, which has no frequency to move", () => {
    // Offering a frequency box for an EchoLink-only net invites a
    // write the server refuses, and describes the net wrongly besides.
    const { container } = render(
      <FrequencyControl
        sessionId="abc"
        connections={[
          {
            id: "00000000-0000-0000-0000-000000001605",
            position: 0,
            kind: "echolink",
            plannedFrequencyHz: null,
            band: null,
            mode: null,
            repeaterOffsetHz: null,
            toneMode: null,
            toneValue: null,
            node: "12345",
            reflector: null,
            network: null,
            talkgroup: null,
            label: null,
            detail: null,
          },
        ]}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("resyncs the prefill when the named connection moves and the field is untouched", () => {
    // A co-owner's change (or this session's own confirmed WS delta) updates
    // the folded frequency while this control is mounted. If the input never
    // resynced, submitting untouched would silently re-post the STALE value
    // and revert the concurrent change.
    const { rerender } = render(
      <FrequencyControl sessionId="abc" connections={ways(14.25)} />,
    );
    expect(screen.getByLabelText(/operating frequency/i)).toHaveValue("14.25");

    rerender(<FrequencyControl sessionId="abc" connections={ways(3.5)} />);

    expect(screen.getByLabelText(/operating frequency/i)).toHaveValue("3.5");
  });

  it("does not clobber an in-progress edit when the connection moves", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <FrequencyControl sessionId="abc" connections={ways(14.25)} />,
    );
    const input = screen.getByLabelText(/operating frequency/i);
    await user.clear(input);
    await user.type(input, "7.100");

    // A concurrent delta arrives while the owner is still typing their own edit.
    rerender(<FrequencyControl sessionId="abc" connections={ways(3.5)} />);

    expect(screen.getByLabelText(/operating frequency/i)).toHaveValue("7.100");
  });

  it("resumes resyncing after a successful submit", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { id: "abc", lifecycle: "live" }));
    vi.stubGlobal("fetch", fetchMock);

    const { rerender } = render(
      <FrequencyControl sessionId="abc" connections={ways(14.25)} />,
    );
    const input = screen.getByLabelText(/operating frequency/i);
    await user.clear(input);
    await user.type(input, "7.200");
    await user.click(screen.getByRole("button", { name: /set frequency/i }));

    // The eventual WS-driven prop update (the authoritative delta folding in)
    // should resync cleanly now that the edit was submitted successfully.
    rerender(<FrequencyControl sessionId="abc" connections={ways(7.2)} />);

    expect(await screen.findByLabelText(/operating frequency/i)).toHaveValue("7.2");
  });
  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(409, {
        type: "/errors/session-already-closed",
        status: 409,
        detail,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(<FrequencyControl sessionId="abc" connections={ways(14.25)} />);
    await user.click(screen.getByRole("button", { name: /set frequency/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/session-already-closed"),
    );
  });

});
