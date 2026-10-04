// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { messageForProblemType } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import { NetNotePanel } from "./NetNotePanel";
import * as sessionApi from "./sessionApi";
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
 * NetNotePanel behavior. Asserts the textarea seeds from the folded
 * `netNote`, and that Save calls `setNetNote` and re-seeds the returned summary —
 * never rendered prose.
 */

function summary(netNote: string | null): SessionSummaryBody {
  return {
    id: "s1",
    definitionId: "d1",
    definitionVersion: 1,
    lifecycle: "live",
    connections: [hfWay(14_250_000)],
    startedAt: null,
    closedAt: null,
    durationSeconds: null,
    latestSeq: 1,
    participantCount: 0,
    workingCheckInId: null,
    netNote,
    controlState: "active",
    roster: [],
    definition: {
      title: "T",
      connections: [hfWay()],
      netCategory: "traffic",
      netType: "open",
    },
  };
}

describe("NetNotePanel", () => {
  it("seeds the textarea from the current net note", () => {
    render(<NetNotePanel sessionId="s1" netNote="Weekly traffic net" onSaved={() => {}} />);
    const textarea = screen.getByLabelText(/net note/i) as HTMLTextAreaElement;
    expect(textarea.value).toBe("Weekly traffic net");
  });

  it("Save calls setNetNote with the edited text and re-seeds the returned summary", async () => {
    const saved = summary("edited note");
    const spy = vi.spyOn(sessionApi, "setNetNote").mockResolvedValue(saved);
    const onSaved = vi.fn();
    render(<NetNotePanel sessionId="s1" netNote={null} onSaved={onSaved} />);

    const textarea = screen.getByLabelText(/net note/i);
    fireEvent.change(textarea, { target: { value: "edited note" } });
    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("s1", "edited note"));
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(saved));
    spy.mockRestore();
  });

  it("resyncs the textarea when netNote changes externally and the operator has not edited it", () => {
    const { rerender } = render(
      <NetNotePanel sessionId="s1" netNote="Round 1 note" onSaved={() => {}} />,
    );
    const textarea = screen.getByLabelText(/net note/i) as HTMLTextAreaElement;
    expect(textarea.value).toBe("Round 1 note");

    // Another operator's `session.note-set` delta folds in and the parent
    // re-renders with the new authoritative netNote — the panel must follow
    // it, not keep showing the stale value forever (review finding: no
    // useEffect resynced `value` to the prop).
    rerender(<NetNotePanel sessionId="s1" netNote="Round 2 note from another operator" onSaved={() => {}} />);
    expect(textarea.value).toBe("Round 2 note from another operator");
  });

  it("does NOT clobber the operator's in-progress edit when netNote changes externally", () => {
    const { rerender } = render(
      <NetNotePanel sessionId="s1" netNote="Round 1 note" onSaved={() => {}} />,
    );
    const textarea = screen.getByLabelText(/net note/i) as HTMLTextAreaElement;

    // The operator starts typing a local draft...
    fireEvent.change(textarea, { target: { value: "my in-progress draft" } });

    // ...and before they Save, another operator's note lands over the wire.
    // The draft must survive — silently overwriting it would be a lost edit.
    rerender(<NetNotePanel sessionId="s1" netNote="someone else's note" onSaved={() => {}} />);
    expect(textarea.value).toBe("my in-progress draft");
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const spy = vi
      .spyOn(sessionApi, "setNetNote")
      .mockRejectedValue(
        new ProblemError({ type: "/errors/note-invalid", status: 422, detail }),
      );

    render(<NetNotePanel sessionId="s1" netNote={null} onSaved={vi.fn()} />);
    fireEvent.change(screen.getByLabelText(/net note/i), {
      target: { value: "too long" },
    });
    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/note-invalid"),
    );
    spy.mockRestore();
  });

});
