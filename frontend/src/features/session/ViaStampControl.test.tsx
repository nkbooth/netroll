// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { ViaStampControl } from "./ViaStampControl";
import { ViaPicker } from "./ViaPicker";
import { createSessionStore } from "./sessionStore";
import type { SessionSummaryBody } from "./sessionWire";
import { UNRESOLVABLE_VIA_LABEL } from "../nets/connectionPresentation";
import type { NetConnection } from "../nets/netsApi";
import { expectNoAxeViolations } from "../../test/axe";

/**
 * The session stamp control. It is the ONE surface
 * that answers "what is this recording?" — the reason an in-memory stamp that
 * does not survive a reload is acceptable at all.
 */

const uuid = (n: number): string =>
  `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

function connection(overrides: Partial<NetConnection> = {}): NetConnection {
  return {
    id: uuid(1),
    position: 0,
    kind: "hf",
    plannedFrequencyHz: 14_230_000,
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
    ...overrides,
  } as NetConnection;
}

const echolink = connection({
  id: uuid(2),
  position: 1,
  kind: "echolink",
  plannedFrequencyHz: null,
  band: null,
  mode: null,
  node: "N1CCK-R",
});

function renderControl(connections: readonly NetConnection[] = [connection(), echolink]) {
  const store = createSessionStore();
  const utils = render(<ViaStampControl store={store} connections={connections} />);
  return { ...utils, store };
}

/** An authoritative reseed whose connection set NO LONGER carries `droppedId` —
 * the owner editing the connection list mid-net, which is the one path that can
 * strand a stamp the operator set against a connection that existed then. */
function summaryWithout(droppedId: string): SessionSummaryBody {
  const kept = [connection(), echolink].filter((c) => c.id !== droppedId);
  return {
    id: uuid(1),
    definitionId: uuid(7),
    definitionVersion: 3,
    lifecycle: "live",
    connections: kept,
    startedAt: "2026-07-16T00:00:00Z",
    closedAt: null,
    durationSeconds: null,
    latestSeq: 2,
    participantCount: 0,
    workingCheckInId: null,
    netNote: null,
    controlState: "active",
    roster: [],
    definition: {
      title: "Sunday Traffic Net",
      connections: kept,
      netCategory: "traffic",
      netType: "open",
    },
  } as unknown as SessionSummaryBody;
}

describe("ViaStampControl — what it says is in force", () => {
  it("says plainly that nothing is stamped before the operator sets one", () => {
    renderControl();
    const status = screen.getByRole("status", { name: /taking check-ins on/i });
    expect(status).toHaveAttribute("data-via-stamp", "none");
  });

  it("names the stamped connection with connectionLabel once one is set", async () => {
    const user = userEvent.setup();
    const { store } = renderControl();

    await user.click(screen.getByRole("radio", { name: "EchoLink — N1CCK-R" }));

    const status = screen.getByRole("status", { name: /taking check-ins on/i });
    expect(status).toHaveAttribute("data-via-stamp", "set");
    expect(status).toHaveTextContent("EchoLink — N1CCK-R");
    expect(store.getState().viaStamp).toEqual({
      kind: "connection",
      connectionId: uuid(2),
    });
  });

  it("offers every connection, including one with no frequency to tune", () => {
    renderControl();
    const group = screen.getByRole("radiogroup", { name: /taking check-ins on/i });
    const labels = within(group)
      .getAllByRole("radio")
      .map((radio) => radio.textContent ?? "");
    expect(labels).toContain("HF — 14.230 MHz");
    expect(labels).toContain("EchoLink — N1CCK-R");
  });

  it("clears back to nothing recorded", async () => {
    const user = userEvent.setup();
    const { store } = renderControl();
    act(() => store.getState().setViaStamp({ kind: "connection", connectionId: uuid(2) }));

    await user.click(screen.getByRole("radio", { name: /^not stamped/i }));

    expect(store.getState().viaStamp).toBeNull();
    expect(screen.getByRole("status", { name: /taking check-ins on/i })).toHaveAttribute(
      "data-via-stamp",
      "none",
    );
  });

  it("never leaves a stranded stamp claiming to be set while nothing is sendable", () => {
    // This used to assert that an unresolvable stamp NAMES itself in words,
    // which it did — but the status line still computed `data-via-stamp="set"`
    // and
    // took the bold style, while `sendableVia` refused that same value, so
    // every check-in that followed recorded no way in. The stranded state
    // should not be worded better; it should not exist.
    //
    // Reached here the way the app reaches it — through `seedFromSnapshot`,
    // which is where the clearing rule lives. Setting the stamp directly to a
    // dangling id (as this test once did) bypasses the only code that can know
    // the connection is gone, so it proves nothing about the shipped path.
    const { store } = renderControl();
    act(() => store.getState().setViaStamp({ kind: "connection", connectionId: uuid(2) }));
    expect(screen.getByRole("status", { name: /taking check-ins on/i })).toHaveAttribute(
      "data-via-stamp",
      "set",
    );

    act(() => store.getState().seedFromSnapshot(summaryWithout(uuid(2))));

    expect(store.getState().viaStamp).toBeNull();
    expect(screen.getByRole("status", { name: /taking check-ins on/i })).toHaveAttribute(
      "data-via-stamp",
      "none",
    );
  });

  it("still names an unresolvable RECORDED way in — only the stamp is cleared", () => {
    // The other half of this behavior, and the reason `UNRESOLVABLE_VIA_LABEL` and
    // `viaLabel`'s branch are NOT dead code: a check-in logged earlier may name
    // a connection the owner has since removed, and that is a true historical
    // fact the picker must keep rendering. Asserted through the picker, which
    // is the surface that renders a recorded value.
    render(
      <ViaPicker
        label="Came in on"
        unsetLabel="Not recorded"
        connections={[connection()]}
        value={{ kind: "connection", connectionId: uuid(0x99) }}
        onChange={() => {}}
      />,
    );

    expect(screen.getAllByText(UNRESOLVABLE_VIA_LABEL).length).toBeGreaterThan(0);
    expect(document.body.textContent).not.toContain(uuid(0x99));
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = renderControl();
    await expectNoAxeViolations(container);
  });
});
