// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { messageForProblemType } from "../../errors/problemMessages";
import { ModerationControls } from "./ModerationControls";
import type { ModerationControlsProps } from "./ModerationControls";
import { ProblemError } from "../auth/authApi";
import type { SessionSummaryBody } from "./sessionWire";

// A session carries its ways IN, not one frequency. The fixtures
// below reach the net on one HF connection unless a test says otherwise.
function hfWay(plannedFrequencyHz = 14_230_000) {
  return {
    id: "00000000-0000-0000-0000-000000001604",
    position: 0,
    kind: "hf",
    plannedFrequencyHz,
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
  };
}


/**
 * Moderation-control tests. Behavior-first: they assert
 * the CAPABILITY gating decision (who sees the control), the `moderateCheckIn`
 * call shape (`block` + `expectedVersion`), the source-conditional block button,
 * and the friendly problem mapping — never rendered copy for its own sake.
 */

const uuid = (n: number): string =>
  `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

function summary(): SessionSummaryBody {
  return {
    id: uuid(1),
    definitionId: uuid(7),
    definitionVersion: 3,
    lifecycle: "live",
    connections: [hfWay(14_250_000)],
    startedAt: "t",
    closedAt: null,
    durationSeconds: null,
    latestSeq: 5,
    participantCount: 0,
    workingCheckInId: null,
    netNote: null,
    controlState: "active",
    roster: [],
    definition: {
      title: "Sunday Traffic Net",
      connections: [hfWay()],
      netCategory: "traffic",
      netType: "open",
    },
  };
}

function renderControls(overrides: Partial<ModerationControlsProps> = {}) {
  const moderateCheckIn = vi.fn().mockResolvedValue(summary());
  const onModerated = vi.fn();
  const props: ModerationControlsProps = {
    sessionId: uuid(1),
    checkInId: uuid(42),
    version: 3,
    source: "self",
    viewerRole: "net-control",
    onModerated,
    deps: { moderateCheckIn },
    ...overrides,
  };
  const result = render(<ModerationControls {...props} />);
  return { moderateCheckIn, onModerated, ...result };
}

describe("ModerationControls", () => {
  it("renders nothing for a viewer without the moderate capability", () => {
    for (const role of ["logger", "relay", "participant"] as const) {
      const { container, unmount } = renderControls({ viewerRole: role });
      expect(container.querySelector("button")).toBeNull();
      unmount();
    }
  });

  it("renders nothing for an account-less (null-role) public viewer", () => {
    const { container } = renderControls({ viewerRole: null });
    expect(container.querySelector("button")).toBeNull();
  });

  it("shows the moderation affordance for net-control and owner", () => {
    for (const role of ["net-control", "owner"] as const) {
      const { unmount } = renderControls({ viewerRole: role });
      expect(screen.getByRole("button", { name: /^remove$/i })).toBeTruthy();
      unmount();
    }
  });

  it("offers Remove & block only for a self-sourced entry", () => {
    const self = renderControls({ source: "self" });
    expect(screen.queryByRole("button", { name: /block/i })).not.toBeNull();
    self.unmount();

    renderControls({ source: "staff" });
    // A staff (account-less) entry has no account to block.
    expect(screen.queryByRole("button", { name: /block/i })).toBeNull();
    // …but plain remove is still offered.
    expect(screen.getByRole("button", { name: /^remove$/i })).toBeTruthy();
  });

  it("calls moderateCheckIn with block:false and the CAS version on Remove", async () => {
    const { moderateCheckIn, onModerated } = renderControls({ version: 7 });
    await userEvent.click(screen.getByRole("button", { name: /^remove$/i }));
    await waitFor(() => {
      expect(moderateCheckIn).toHaveBeenCalledWith(uuid(1), uuid(42), {
        block: false,
        expectedVersion: 7,
      });
    });
    expect(onModerated).toHaveBeenCalledTimes(1);
  });

  it("calls moderateCheckIn with block:true on Remove & block", async () => {
    const { moderateCheckIn } = renderControls({ version: 4, source: "self" });
    await userEvent.click(screen.getByRole("button", { name: /block/i }));
    await waitFor(() => {
      expect(moderateCheckIn).toHaveBeenCalledWith(uuid(1), uuid(42), {
        block: true,
        expectedVersion: 4,
      });
    });
  });

  it("maps an account-blocked problem to a friendly message (no raw slug)", async () => {
    const moderateCheckIn = vi
      .fn()
      .mockRejectedValue(new ProblemError({ type: "/errors/nothing-to-block", status: 422 }));
    render(
      <ModerationControls
        sessionId={uuid(1)}
        checkInId={uuid(42)}
        version={1}
        source="self"
        viewerRole="net-control"
        onModerated={vi.fn()}
        deps={{ moderateCheckIn }}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: /block/i }));
    const alert = await screen.findByRole("alert");
    // A user-facing message, never the raw slug.
    expect(alert.textContent).not.toContain("/errors/");
    expect(alert.textContent).toBeTruthy();
  });

  it("still surfaces a generic alert when the failure is not a ProblemError", async () => {
    // A non-ProblemError rejection (a network failure, a thrown TypeError,
    // etc.) must still render feedback — not silently do nothing. The `problem`
    // sentinel has three states (null/undefined/Problem); overloading
    // `undefined` for both "no error yet" and "unknown error" would make this
    // case indistinguishable from the initial render and swallow the failure.
    renderControls({
      deps: { moderateCheckIn: vi.fn().mockRejectedValue(new TypeError("network down")) },
    });
    await userEvent.click(screen.getByRole("button", { name: /^remove$/i }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBeTruthy();
  });
  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const moderateCheckIn = vi
      .fn()
      .mockRejectedValue(
        new ProblemError({ type: "/errors/nothing-to-block", status: 422, detail }),
      );
    renderControls({ deps: { moderateCheckIn } });

    await userEvent.click(screen.getByRole("button", { name: /block/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/nothing-to-block"),
    );
  });

});
