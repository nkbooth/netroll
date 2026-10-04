// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { messageForProblemType } from "../../errors/problemMessages";
import { CheckInDetailModal } from "./CheckInDetailModal";
import type { CheckInDetailModalDeps } from "./CheckInDetailModal";
import { ProblemError } from "../auth/authApi";
import { createSessionStore } from "./sessionStore";
import type { DisplayRosterEntry } from "./sessionStore";
import type { SessionSummaryBody } from "./sessionWire";
import { expectNoAxeViolations } from "../../test/axe";

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
 * Detail-modal tests. Behavior-first: they
 * assert the lock acquire/heartbeat/release calls, the `updateCheckIn`/
 * `removeCheckIn` payloads (incl. `expectedVersion`), the store re-seed, the Esc/
 * Cancel close, the focus trap, and the 409 problem mapping — never copy beyond
 * the holder callsign an operator must read.
 */

const uuid = (n: number): string =>
  `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

function entry(overrides: Partial<DisplayRosterEntry> = {}): DisplayRosterEntry {
  return {
    key: uuid(42),
    callsign: "W1AW",
    optimistic: false,
    addedAt: "2026-07-16T00:00:00Z",
    addedBy: uuid(200),
    source: "staff",
    signalReport: null,
    staying: "in-and-out",
    name: null,
    location: null,
    grid: null,
    precedence: "routine",
    traffic: null,
    notes: null,
    publicNote: null,
    via: null,
    viaLabel: null,
    relayedBy: null,
    working: false,
    worked: false,
    version: 3,
    corrections: [],
    lockedBy: null,
    ...overrides,
  };
}

/** A store seeded with the session's frozen connection set — what the way-in
 * editor resolves its options against. */
function seededStore() {
  const store = createSessionStore();
  store.getState().seedFromSnapshot(summary());
  return store;
}

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
    latestSeq: 4,
    participantCount: 1,
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

function makeDeps(overrides: Partial<CheckInDetailModalDeps> = {}): Required<CheckInDetailModalDeps> {
  return {
    acquireLock: vi.fn().mockResolvedValue({ holderCallsign: "W1AW", expiresAt: "t" }),
    releaseLock: vi.fn().mockResolvedValue(undefined),
    updateCheckIn: vi.fn().mockResolvedValue(summary()),
    removeCheckIn: vi.fn().mockResolvedValue(summary()),
    // A no-op heartbeat scheduler by default; tests that exercise it override.
    scheduleHeartbeat: vi.fn().mockReturnValue(() => {}),
    ...overrides,
  };
}

const setModalWidth = (width: number): void => {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: width,
  });
};

afterEach(() => {
  setModalWidth(1024);
});

describe("CheckInDetailModal", () => {
  it("acquires the soft-lock when it opens", async () => {
    const deps = makeDeps();
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={() => {}} deps={deps} />,
    );
    await waitFor(() => expect(deps.acquireLock).toHaveBeenCalledWith(uuid(1), uuid(42)));
  });

  it("Save commits the full field set with expectedVersion, re-seeds the store, and closes", async () => {
    const deps = makeDeps();
    const store = createSessionStore();
    const onClose = vi.fn();
    const seed = vi.spyOn(store.getState(), "seedFromSnapshot");
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={store} onClose={onClose} deps={deps} />,
    );

    await userEvent.clear(screen.getByLabelText(/callsign/i));
    await userEvent.type(screen.getByLabelText(/callsign/i), "W1AX");
    await userEvent.type(screen.getByLabelText(/name/i), "Maria");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(deps.updateCheckIn).toHaveBeenCalledWith(uuid(1), uuid(42), {
        callsign: "W1AX",
        name: "Maria",
        location: "",
        grid: "",
        // An emptied report/traffic is sent as an explicit `null`,
        // never dropped. The server now reads an ABSENT key as "keep what is
        // stored", so omitting these would turn a deliberate clear into a no-op.
        signalReport: null,
        staying: "in-and-out",
        precedence: "routine",
        traffic: null,
        notes: "",
        // The SPLIT field. Both notes ride the same save, with the
        // same PUT-replace empty-string-clears contract.
        publicNote: "",
        expectedVersion: 3,
      }),
    );
    expect(seed).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("Save carries the selected precedence and typed traffic count", async () => {
    const deps = makeDeps();
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={() => {}} deps={deps} />,
    );

    await userEvent.click(screen.getByRole("radio", { name: "Emergency" }));
    await userEvent.type(screen.getByLabelText(/traffic count/i), "3");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(deps.updateCheckIn).toHaveBeenCalledWith(
        uuid(1),
        uuid(42),
        expect.objectContaining({ precedence: "emergency", traffic: 3 }),
      ),
    );
  });

  it("emptying a previously-set report and traffic clears them rather than leaving them alone", async () => {
    // The regression contract from the client side: after the server started
    // reading an absent key as "keep", the modal is the only thing that can
    // distinguish "the operator wiped this box" from "this UI never asked about
    // this field". A deliberate clear must reach the wire as an explicit null.
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({ signalReport: "599", traffic: 3 })}
        store={createSessionStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );

    await userEvent.clear(screen.getByLabelText(/signal report/i));
    await userEvent.clear(screen.getByLabelText(/traffic count/i));
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(deps.updateCheckIn).toHaveBeenCalledWith(
        uuid(1),
        uuid(42),
        expect.objectContaining({ signalReport: null, traffic: null }),
      ),
    );
  });

  it("seeds the STAFF note textarea and carries the edited note into updateCheckIn", async () => {
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({ notes: "prior note" })}
        store={createSessionStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );
    // The box an operator already knows is now labelled as the
    // PRIVATE one — everything they wrote before this story is still in it, and
    // the label is how they learn that without reading a changelog.
    const notesField = screen.getByLabelText(/staff note/i) as HTMLTextAreaElement;
    expect(notesField.value).toBe("prior note");
    await userEvent.clear(notesField);
    await userEvent.type(notesField, "handling traffic");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(deps.updateCheckIn).toHaveBeenCalledWith(
        uuid(1),
        uuid(42),
        expect.objectContaining({ notes: "handling traffic" }),
      ),
    );
  });

  it("seeds the PUBLIC note in its own field and carries it into updateCheckIn", async () => {
    // Two fields, two audiences, one save path. Editing the public one
    // must not disturb the staff one — that independence is the whole point of
    // splitting the field rather than widening its visibility.
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({ notes: "prior staff note", publicNote: "prior public note" })}
        store={createSessionStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );
    const publicField = screen.getByLabelText(/public note/i) as HTMLTextAreaElement;
    expect(publicField.value).toBe("prior public note");
    await userEvent.clear(publicField);
    await userEvent.type(publicField, "relaying for W1BBB");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(deps.updateCheckIn).toHaveBeenCalledWith(
        uuid(1),
        uuid(42),
        expect.objectContaining({
          publicNote: "relaying for W1BBB",
          notes: "prior staff note",
        }),
      ),
    );
  });

  it("seeds the precedence selector from the entry", async () => {
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({ precedence: "priority", traffic: 5 })}
        store={createSessionStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );
    expect(screen.getByRole("radio", { name: "Priority" }).getAttribute("aria-checked")).toBe(
      "true",
    );
    expect((screen.getByLabelText(/traffic count/i) as HTMLInputElement).value).toBe("5");
  });

  it("Esc cancels without a write and releases the lock", async () => {
    const deps = makeDeps();
    const onClose = vi.fn();
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={onClose} deps={deps} />,
    );
    await waitFor(() => expect(deps.acquireLock).toHaveBeenCalled());

    await userEvent.keyboard("{Escape}");

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(deps.updateCheckIn).not.toHaveBeenCalled();
    // The mount cleanup releases the lease best-effort.
    await waitFor(() => expect(deps.releaseLock).toHaveBeenCalledWith(uuid(1), uuid(42)));
  });

  it("Cancel closes without a write", async () => {
    const deps = makeDeps();
    const onClose = vi.fn();
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={onClose} deps={deps} />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(deps.updateCheckIn).not.toHaveBeenCalled();
  });

  it("Remove tombstones the entry with expectedVersion, re-seeds, and closes", async () => {
    const deps = makeDeps();
    const store = createSessionStore();
    const onClose = vi.fn();
    const seed = vi.spyOn(store.getState(), "seedFromSnapshot");
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={store} onClose={onClose} deps={deps} />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(deps.removeCheckIn).toHaveBeenCalledWith(uuid(1), uuid(42), 3));
    expect(seed).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("surfaces the stale-version 409 as a mapped message and does NOT close", async () => {
    const deps = makeDeps({
      updateCheckIn: vi
        .fn()
        .mockRejectedValue(new ProblemError({ type: "/errors/stale-version", status: 409 })),
    });
    const onClose = vi.fn();
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={onClose} deps={deps} />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());
    expect(onClose).not.toHaveBeenCalled();
  });

  it("shows the holder's callsign and disables Save/Remove when the lock is held on open", async () => {
    const deps = makeDeps({
      acquireLock: vi
        .fn()
        .mockRejectedValue(new ProblemError({ type: "/errors/lock-held", status: 409, detail: "W2BCD" })),
    });
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={() => {}} deps={deps} />,
    );
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("W2BCD is editing this entry."),
    );
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Remove" })).toBeDisabled();
    // Cancel must stay available so the operator can back out.
    expect(screen.getByRole("button", { name: "Cancel" })).not.toBeDisabled();
  });

  it("surfaces a heartbeat renewal's lock-held 409 (lease lost mid-edit) and disables Save/Remove", async () => {
    const heartbeats: Array<() => void> = [];
    const deps = makeDeps({
      scheduleHeartbeat: vi.fn().mockImplementation((fn: () => void) => {
        heartbeats.push(fn);
        return () => {};
      }),
    });
    // The first acquireLock (on open) succeeds; the second (the heartbeat
    // renewal) loses the lease to another operator.
    deps.acquireLock = vi
      .fn()
      .mockResolvedValueOnce({ holderCallsign: "W1AW", expiresAt: "t" })
      .mockRejectedValueOnce(
        new ProblemError({ type: "/errors/lock-held", status: 409, detail: "W2BCD" }),
      );
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={() => {}} deps={deps} />,
    );
    await waitFor(() => expect(deps.acquireLock).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("button", { name: "Save" })).not.toBeDisabled();

    heartbeats[0]?.();

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("W2BCD is editing this entry."),
    );
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Remove" })).toBeDisabled();
  });

  it("collapses the field grid to a single column at/below 560px", () => {
    setModalWidth(560);
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={() => {}} deps={makeDeps()} />,
    );
    const body = screen.getByTestId("checkin-modal-body");
    expect(body).toHaveAttribute("data-columns", "one");
    expect(body).toHaveStyle({ gridTemplateColumns: "1fr" });
  });

  it("lays the field grid out in two columns above 560px", () => {
    setModalWidth(900);
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={() => {}} deps={makeDeps()} />,
    );
    const body = screen.getByTestId("checkin-modal-body");
    expect(body).toHaveAttribute("data-columns", "two");
    expect(body).toHaveStyle({ gridTemplateColumns: "repeat(2, 1fr)" });
  });

  it("renews the lease on the heartbeat while open", async () => {
    const heartbeats: Array<() => void> = [];
    const deps = makeDeps({
      scheduleHeartbeat: vi.fn().mockImplementation((fn: () => void) => {
        heartbeats.push(fn);
        return () => {};
      }),
    });
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={() => {}} deps={deps} />,
    );
    await waitFor(() => expect(deps.acquireLock).toHaveBeenCalledTimes(1));
    // Firing the heartbeat re-acquires (renews) the lease.
    heartbeats[0]?.();
    await waitFor(() => expect(deps.acquireLock).toHaveBeenCalledTimes(2));
  });

  it("is a labelled modal dialog with the fields for editing", () => {
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({ signalReport: "599", name: "Maria" })}
        store={createSessionStore()}
        onClose={() => {}}
        deps={makeDeps()}
      />,
    );
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(screen.getByLabelText(/callsign/i)).toHaveValue("W1AW");
    expect(screen.getByLabelText(/name/i)).toHaveValue("Maria");
  });

  it("seeds the grid input from the entry and sends the typed grid on Save", async () => {
    // A full round trip through the modal's own state. Asserted on
    // the `updateCheckIn` CALL ARGUMENT, never on rendered copy.
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({ grid: "FN31" })}
        store={createSessionStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );
    const input = screen.getByLabelText(/^grid$/i);
    expect(input).toHaveValue("FN31");

    await userEvent.clear(input);
    await userEvent.type(input, "FN42");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(deps.updateCheckIn).toHaveBeenCalledWith(
        uuid(1),
        uuid(42),
        expect.objectContaining({ grid: "FN42" }),
      ),
    );
  });

  it("clears the grid by sending an empty string, not undefined (PUT-replace)", async () => {
    // The server treats an omitted key as "clear" too, but sending "" is the
    // deliberate house convention shared with name/location/notes — and it is
    // exactly why `parse_edit_grid` needs its blank-after-trim arm.
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({ grid: "FN31" })}
        store={createSessionStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );
    await userEvent.clear(screen.getByLabelText(/^grid$/i));
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(deps.updateCheckIn).toHaveBeenCalledWith(
        uuid(1),
        uuid(42),
        expect.objectContaining({ grid: "" }),
      ),
    );
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={createSessionStore()} onClose={() => {}} deps={makeDeps()} />,
    );
    await expectNoAxeViolations(container);
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. `/errors/lock-held` is the one
    // slug this modal must NOT order that way — see the holder-callsign test
    // above, which is the regression guard for the composition's relocation.
    const detail = "the field-naming answer the server sent";
    const deps = makeDeps({
      updateCheckIn: vi
        .fn()
        .mockRejectedValue(
          new ProblemError({ type: "/errors/stale-version", status: 409, detail }),
        ),
    });
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry()}
        store={createSessionStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/stale-version"),
    );
  });


  it("renders the way in as a LABEL, never as the id it is keyed by", async () => {
    // This used to pin "always omitted", because the modal had no editor and
    // sending `via: null` on every unrelated save would clear a recorded way in
    // — the field-wipe defect, one field over. That hazard is REAL and has not
    // gone away; it is now guarded by the tri-state below, whose untouched arm
    // makes the same assertion. What survives here is the other half: the label
    // reaches the operator and the UUID does not.
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({
          via: { kind: "connection", connectionId: hfWay().id },
          viaLabel: "HF — 14.230 MHz",
        })}
        store={seededStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );

    // 14.250, not the 14.230 the ROW carries: the editor resolves against the
    // session's connection set as it stands now, so a mid-net QSY re-labels
    // what the operator is looking at rather than showing a frozen string.
    expect(screen.getByText("HF — 14.250 MHz")).toBeTruthy();
    expect(document.body.textContent).not.toContain(hfWay().id);
  });
});

describe("CheckInDetailModal — editing the way in", () => {
  /** Saves and returns the options object `updateCheckIn` was handed. */
  async function saveAndReadOptions(
    deps: CheckInDetailModalDeps,
  ): Promise<Record<string, unknown>> {
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(deps.updateCheckIn).toHaveBeenCalledTimes(1));
    return vi.mocked(deps.updateCheckIn as NonNullable<typeof deps.updateCheckIn>).mock
      .calls[0][2] as unknown as Record<string, unknown>;
  }

  it("OMITS `via` when the operator did not touch it — an unrelated save clears nothing", async () => {
    // The hazard pin was guarding, kept as the untouched arm: an
    // absent key means "keep the stored value", and sending `null` here on
    // every save would erase a way in nobody was editing.
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({
          via: { kind: "connection", connectionId: hfWay().id },
          viaLabel: "HF — 14.250 MHz",
        })}
        store={seededStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );

    // A change to a DIFFERENT field, to prove the omission is about `via` and
    // not about the save being a no-op.
    await userEvent.type(screen.getByLabelText("Name"), "Maria");

    const sent = await saveAndReadOptions(deps);
    expect(Object.hasOwn(sent, "via")).toBe(false);
    expect(sent.name).toBe("Maria");
  });

  it("sends an explicit `null` when the operator CLEARS a recorded way in", async () => {
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({
          via: { kind: "connection", connectionId: hfWay().id },
          viaLabel: "HF — 14.250 MHz",
        })}
        store={seededStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );

    await userEvent.click(screen.getByRole("radio", { name: /not recorded/i }));

    const sent = await saveAndReadOptions(deps);
    expect(Object.hasOwn(sent, "via")).toBe(true);
    expect(sent.via).toBeNull();
  });

  it("sends the new VALUE when the operator changes the way in", async () => {
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry()}
        store={seededStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );

    await userEvent.click(screen.getByRole("radio", { name: "HF — 14.250 MHz" }));

    const sent = await saveAndReadOptions(deps);
    expect(sent.via).toEqual({ kind: "connection", connectionId: hfWay().id });
  });

  it("sends free text through the ORDINARY edit path — no correction event", async () => {
    const deps = makeDeps();
    render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry()}
        store={seededStore()}
        onClose={() => {}}
        deps={deps}
      />,
    );

    await userEvent.click(screen.getByRole("radio", { name: /other/i }));
    await userEvent.type(screen.getByRole("textbox", { name: /way in/i }), "Bob's hotspot");

    const sent = await saveAndReadOptions(deps);
    expect(sent.via).toEqual({ kind: "unlisted", text: "Bob's hotspot" });
    expect(sent.expectedVersion).toBe(3);
  });

  it("has no WCAG 2.1 AA violations with the way-in editor mounted", async () => {
    const { container } = render(
      <CheckInDetailModal
        sessionId={uuid(1)}
        entry={entry({
          via: { kind: "connection", connectionId: hfWay().id },
          viaLabel: "HF — 14.250 MHz",
        })}
        store={seededStore()}
        onClose={() => {}}
        deps={makeDeps()}
      />,
    );
    await expectNoAxeViolations(container);
  });
});

describe("CheckInDetailModal — correcting the relaying station", () => {
  it("OMITS relayedBy from the PUT when the operator never touched it", async () => {
    // The tri-state: omitted means KEEP. Sending `null` on every
    // unrelated save would erase a relaying station the operator never opened
    // the field for.
    const deps = makeDeps();
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry({ relayedBy: "W3REL" })} store={seededStore()} onClose={() => {}} deps={deps} />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(deps.updateCheckIn).toHaveBeenCalled());
    const body = vi.mocked(deps.updateCheckIn).mock.calls[0]?.[2] as unknown as Record<string, unknown>;
    expect(Object.hasOwn(body, "relayedBy")).toBe(false);
  });

  it("sends the typed callsign when the operator sets one", async () => {
    const deps = makeDeps();
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry()} store={seededStore()} onClose={() => {}} deps={deps} />,
    );
    await userEvent.type(screen.getByLabelText(/relayed by/i), "W3REL");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(deps.updateCheckIn).toHaveBeenCalled());
    const body = vi.mocked(deps.updateCheckIn).mock.calls[0]?.[2] as unknown as Record<string, unknown>;
    expect(body.relayedBy).toBe("W3REL");
  });

  it("sends an explicit null when the operator empties one that was set", async () => {
    // This is the exception-path surface where a mis-entry gets removed, so a
    // cleared field must reach the server as a CLEAR and not as an omission.
    const deps = makeDeps();
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry({ relayedBy: "W3REL" })} store={seededStore()} onClose={() => {}} deps={deps} />,
    );
    await userEvent.clear(screen.getByLabelText(/relayed by/i));
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(deps.updateCheckIn).toHaveBeenCalled());
    const body = vi.mocked(deps.updateCheckIn).mock.calls[0]?.[2] as unknown as Record<string, unknown>;
    expect(body.relayedBy).toBeNull();
  });

  it("prefills the field from the entry so an untouched save cannot rewrite it", () => {
    render(
      <CheckInDetailModal sessionId={uuid(1)} entry={entry({ relayedBy: "W3REL" })} store={seededStore()} onClose={() => {}} deps={makeDeps()} />,
    );
    expect((screen.getByLabelText(/relayed by/i) as HTMLInputElement).value).toBe("W3REL");
  });
});
