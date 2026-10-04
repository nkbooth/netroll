// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { UNRESOLVABLE_VIA_LABEL } from "../nets/connectionPresentation";
import { RosterEntry } from "./RosterEntry";
import { createSessionStore, selectRoster } from "./sessionStore";
import type { DisplayRosterEntry } from "./sessionStore";
import { expectNoAxeViolations } from "../../test/axe";

/** A DisplayRosterEntry with the optional fields defaulted, for terse literals. */
function entry(overrides: Partial<DisplayRosterEntry> & { key: string; callsign: string }): DisplayRosterEntry {
  return {
    optimistic: false,
    addedAt: "2026-07-16T00:00:00Z",
    addedBy: null,
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
    version: 1,
    corrections: [],
    lockedBy: null,
    ...overrides,
  };
}

describe("RosterEntry", () => {
  it("holds the report column open with an em-dash when a station has none yet", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "VE3MMQ", signalReport: null })}
          showReport
        />
      </ul>,
    );

    // Dropping the cell entirely shifts every following column left on that
    // row, so a report-bearing surface keeps the slot and marks it empty.
    const cell = screen.getByTestId("report-cell");
    expect(cell).toHaveTextContent("—");
  });

  it("renders no report slot at all on a surface that does not show reports", () => {
    render(
      <ul>
        <RosterEntry entry={entry({ key: "a", callsign: "VE3MMQ" })} />
      </ul>,
    );

    // The public participant view has no Report column, so there is no slot to
    // hold open — an em-dash there would invent a column.
    expect(screen.queryByTestId("report-cell")).not.toBeInTheDocument();
  });

  it("renders the callsign in the monospace radio-data face", () => {
    render(
      <ul>
        <RosterEntry entry={entry({ key: "a", callsign: "W1AW" })} />
      </ul>,
    );
    const call = screen.getByText("W1AW");
    expect(call.className).toContain("mono");
  });

  it("flags an optimistic (not-yet-echoed) entry distinctly", () => {
    render(
      <ul>
        <RosterEntry entry={entry({ key: "b", callsign: "K2ABC", optimistic: true })} />
      </ul>,
    );
    expect(screen.getByRole("listitem").getAttribute("data-optimistic")).toBe("true");
  });

  it("marks an authoritative entry as not optimistic", () => {
    render(
      <ul>
        <RosterEntry entry={entry({ key: "c", callsign: "N1CCK" })} />
      </ul>,
    );
    expect(screen.getByRole("listitem").getAttribute("data-optimistic")).toBe("false");
  });

  it("renders a Staff-entered source badge on the operator surface when showSource is set", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", source: "staff" })}
          showSource
        />
      </ul>,
    );
    const badge = screen.getByText(/staff/i).closest("[data-source]");
    expect(badge?.getAttribute("data-source")).toBe("staff");
  });

  it("renders the Self badge variant when the entry carries source=self (component-level)", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "b", callsign: "K2ABC", source: "self" })}
          showSource
        />
      </ul>,
    );
    expect(screen.getByText(/self/i).closest("[data-source]")?.getAttribute("data-source")).toBe(
      "self",
    );
  });

  it("renders the precedence chip and traffic count on the operator surface", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "e", callsign: "W1AW", precedence: "emergency", traffic: 3 })}
          showSource
        />
      </ul>,
    );
    const chip = screen.getByText("Emergency").closest("[data-precedence]");
    expect(chip?.getAttribute("data-precedence")).toBe("emergency");
    // AMENDED. The count used to render as a bare "3"; it is
    // now labelled on every surface that shows it, this console included, so the
    // assertion moves from an exact-text lookup to the cell's own marker.
    const count = document.querySelector("[data-traffic]");
    expect(count).not.toBeNull();
    expect(count?.textContent).toContain("3");
  });

  it("renders NO precedence chip or traffic when neither showSource nor showPrecedence is set", () => {
    // This test used to be named "on the public surface (showSource omitted)".
    // That equivalence is false in BOTH directions: the public roster passes
    // `showPrecedence` without `showSource`, and `showSource` omitted does not
    // mean "public".
    // What the cell has always actually been gated on is the pair of props, so
    // that is what the test now names.
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "e", callsign: "W1AW", precedence: "emergency", traffic: 3 })}
        />
      </ul>,
    );
    expect(screen.queryByText("Emergency")).toBeNull();
    // The count is asserted through the cell's own marker, not a text lookup.
    // `queryByText("3")` was VACUOUS here: the labelled cell renders "3 traffic"
    // in one span and Testing Library's string matcher is exact-full-match after
    // whitespace normalization, so it matched nothing whether the cell rendered
    // or not (measured 2026-08-29 — the assertion passed with `showPrecedence`
    // set and the cell present). Its positive twin above was moved to
    // `[data-traffic]` for exactly this reason; the negative half is now too.
    expect(document.querySelector("[data-traffic]")).toBeNull();
  });

  it("renders the precedence chip and a LABELLED traffic count under showPrecedence alone", () => {
    // The count was already on the operator row as
    // a bare unlabelled integer under a head strip that says only "Precedence".
    // An observer who did not type the number cannot tell a traffic count from a
    // signal report or a list position, so the number is now labelled wherever it
    // renders — the operator console included.
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "e", callsign: "W1AW", precedence: "emergency", traffic: 3 })}
          showPrecedence
        />
      </ul>,
    );
    const count = document.querySelector("[data-traffic]");
    expect(count).not.toBeNull();
    expect(count?.textContent).toContain("3");
    // The label is what makes it legible. A screen-reader-only label would not
    // serve the person who did not type the number and is looking at it, so the
    // label must be VISIBLE as well as announced.
    expect(count?.textContent).toMatch(/traffic/i);
    expect(count?.getAttribute("aria-label")).toMatch(/traffic/i);
  });

  it("renders the staying indicator under showStaying without the rest of the staff surface", () => {
    // `showStaying` is a NEW prop beside `showReport` /
    // `showPrecedence`. Widening `showSource` instead would drag the soft-lock
    // flag, the correction annotations, the working-cursor control and the Edit
    // affordance onto the public roster with it.
    render(
      <ul>
        <RosterEntry
          entry={entry({
            key: "e",
            callsign: "W1AW",
            staying: "staying-for-comments",
            signalReport: "599",
          })}
          showStaying
          onEdit={() => {}}
        />
      </ul>,
    );
    expect(document.querySelector("[data-staying]")).not.toBeNull();
    // None of `showSource`'s other affordances came along.
    expect(screen.queryByRole("button", { name: /edit/i })).toBeNull();
    expect(screen.queryByTestId("report-cell")).toBeNull();
  });

  it("renders the public note on any surface that carries one, and never the staff note", () => {
    // The per-station note SPLIT. The staff note is the
    // operator's private channel and crosses no roster row; the public note is
    // written for the observer and renders on the row.
    render(
      <ul>
        <RosterEntry
          entry={entry({
            key: "e",
            callsign: "W1AW",
            notes: "STAFF-ONLY: sounded rough",
            publicNote: "relaying for W1BBB",
            via: null,
          })}
        />
      </ul>,
    );
    expect(document.querySelector("[data-public-note]")?.textContent).toContain(
      "relaying for W1BBB",
    );
    expect(document.body.textContent).not.toContain("STAFF-ONLY");
  });

  it("renders a precedence correction annotation from derived state", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({
            key: "e",
            callsign: "W1AW",
            precedence: "emergency",
            corrections: [
              { field: "precedence", from: "routine", to: "emergency", at: "2026-07-16T00:00:03Z" },
            ],
          })}
          showSource
        />
      </ul>,
    );
    // The annotation is driven by the derived correction, not the event payload.
    expect(screen.getByText(/Correcting precedence/i).getAttribute("data-correction")).toBe(
      "precedence",
    );
  });

  it("renders NO badge on the public surface (showSource omitted) even if a source is present", () => {
    render(
      <ul>
        <RosterEntry entry={entry({ key: "c", callsign: "W1AW", source: "staff" })} />
      </ul>,
    );
    expect(screen.queryByText(/staff/i)).not.toBeInTheDocument();
    expect(document.querySelector("[data-source]")).toBeNull();
  });

  it("renders the staying indicator (color+icon+label) on the operator surface", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", staying: "staying-for-comments" })}
          showSource
        />
      </ul>,
    );
    const indicator = screen.getByText("Staying").closest("[data-staying]");
    expect(indicator?.getAttribute("data-staying")).toBe("staying-for-comments");
    expect(indicator?.querySelector("svg")).not.toBeNull();
  });

  it("renders the report verbatim in mono on the operator surface", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", signalReport: "599" })}
          showSource
        />
      </ul>,
    );
    const report = screen.getByText("599");
    expect(report.className).toContain("mono");
  });

  it("renders NO report without showReport and NO staying indicator without showStaying", () => {
    // The old name asserted that "showSource omitted" WAS the public surface;
    // the public surface passes `showStaying` and `showPrecedence` without
    // `showSource`, so that name taught the next reader the wrong gate. The behaviour under test — each cell
    // is gated on its OWN prop — is unchanged and still asserted.
    render(
      <ul>
        <RosterEntry
          entry={entry({
            key: "a",
            callsign: "W1AW",
            signalReport: "599",
            staying: "staying-for-comments",
          })}
        />
      </ul>,
    );
    expect(screen.queryByText("599")).not.toBeInTheDocument();
    expect(document.querySelector("[data-staying]")).toBeNull();
    expect(screen.queryByText("Staying")).not.toBeInTheDocument();
  });

  it("keeps the staying indicator's color + icon + LABEL treatment on the public roster", () => {
    // Operators watch on a phone in the dark, so
    // the indicator is never reduced to a dot or a bare color swatch because the
    // public row is denser. The public surface gets the same structural
    // treatment the console has.
    const { container } = render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", staying: "staying-for-comments" })}
          showStaying
        />
      </ul>,
    );
    const indicator = container.querySelector("[data-staying]");
    expect(indicator).not.toBeNull();
    expect(indicator?.textContent?.trim().length ?? 0).toBeGreaterThan(0);
    expect(indicator?.querySelector("svg")).not.toBeNull();
  });

  it("renders an Edit affordance on the operator surface that invokes onEdit", async () => {
    const onEdit = vi.fn();
    render(
      <ul>
        <RosterEntry entry={entry({ key: "a", callsign: "W1AW" })} showSource onEdit={onEdit} />
      </ul>,
    );
    const button = screen.getByRole("button", { name: /edit w1aw/i });
    await userEvent.click(button);
    expect(onEdit).toHaveBeenCalledTimes(1);
  });

  it("renders NO Edit affordance on the public surface (showSource omitted)", () => {
    const onEdit = vi.fn();
    render(
      <ul>
        <RosterEntry entry={entry({ key: "a", callsign: "W1AW" })} onEdit={onEdit} />
      </ul>,
    );
    expect(screen.queryByRole("button", { name: /edit/i })).not.toBeInTheDocument();
  });

  it("marks a row locked-by-another read-only: data-locked, an editing flag, and a disabled Edit", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", lockedBy: "W2BCD" })}
          showSource
          onEdit={() => {}}
        />
      </ul>,
    );
    expect(screen.getByRole("listitem").getAttribute("data-locked")).toBe("true");
    // The lock flag is present (structural, not asserting exact copy beyond the
    // holder callsign the operator must see).
    expect(document.querySelector("[data-lock-flag]")?.textContent).toContain("W2BCD");
    expect(screen.getByRole("button", { name: /edit w1aw/i })).toBeDisabled();
  });

  it("renders a correction annotation per changed field (derived, structural)", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({
            key: "a",
            callsign: "W1AW",
            corrections: [
              { field: "callsign", from: "W1AV", to: "W1AW", at: "t" },
              { field: "signal-report", from: null, to: "599", at: "t" },
            ],
          })}
          showSource
        />
      </ul>,
    );
    expect(document.querySelector('[data-correction="callsign"]')).not.toBeNull();
    expect(document.querySelector('[data-correction="signal-report"]')).not.toBeNull();
  });

  it("renders the WorkingCursor treatment on a working row", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", working: true })}
          showSource
          onSetWorking={() => {}}
        />
      </ul>,
    );
    expect(screen.getByText(/working now/i)).toBeInTheDocument();
    // The row is flagged working for the left-bar/wash treatment.
    expect(screen.getByRole("listitem").getAttribute("data-working")).toBe("true");
  });

  it("renders the worked-dim + green tick on a completed (worked, not working) row", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", worked: true, working: false })}
          showSource
          onSetWorking={() => {}}
        />
      </ul>,
    );
    const li = screen.getByRole("listitem");
    expect(li.getAttribute("data-worked")).toBe("true");
    // The worked tick has an accessible label (color+icon+label, never color alone).
    expect(screen.getByLabelText(/worked/i)).toBeInTheDocument();
  });

  it("renders NEITHER the working nor worked treatment on the public surface", () => {
    render(
      <ul>
        <RosterEntry entry={entry({ key: "a", callsign: "W1AW", working: true, worked: true })} />
      </ul>,
    );
    expect(screen.queryByText(/working now/i)).toBeNull();
    expect(screen.queryByLabelText(/worked/i)).toBeNull();
  });

  it("the inline set-working control invokes onSetWorking with the row key", async () => {
    const onSetWorking = vi.fn();
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "ci-1", callsign: "W1AW" })}
          showSource
          onSetWorking={onSetWorking}
        />
      </ul>,
    );
    await userEvent.click(screen.getByRole("button", { name: /set as working station/i }));
    expect(onSetWorking).toHaveBeenCalledWith("ci-1");
  });

  it("selecting the row (click/focus) invokes onSelect with the row key", async () => {
    const onSelect = vi.fn();
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "ci-9", callsign: "W1AW" })}
          showSource
          onSelect={onSelect}
        />
      </ul>,
    );
    const li = screen.getByRole("listitem");
    expect(li.getAttribute("tabindex")).toBe("0");
    await userEvent.click(li);
    expect(onSelect).toHaveBeenCalledWith("ci-9");
  });

  it("an optimistic (not-yet-echoed) row is not selectable as the w-hotkey target", () => {
    // Review finding: the inline set-working control already excludes
    // optimistic rows (they have no real checkInId yet), but row selection
    // (which drives the `w` hotkey in LiveSessionPage) had no matching guard —
    // selecting a pending row and pressing `w` would send a bogus id to the
    // server. A still-pending row must not be focusable/clickable-selectable.
    const onSelect = vi.fn();
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "pending-1", callsign: "W1AW", optimistic: true })}
          showSource
          onSelect={onSelect}
        />
      </ul>,
    );
    const li = screen.getByRole("listitem");
    expect(li.getAttribute("tabindex")).toBeNull();
    li.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("renders an absolute local Checked time instead of the relative Heard phrase when heardAbsolute is set (post-net summary)", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", addedAt: "2026-07-16T00:00:00Z" })}
          heardAbsolute
        />
      </ul>,
    );
    expect(screen.queryByText(/ago|in \d/i)).not.toBeInTheDocument();
    expect(screen.getByText(new Date("2026-07-16T00:00:00Z").toLocaleString())).toBeInTheDocument();
  });

  it("renders the Report and Precedence cells via showReport/showPrecedence WITHOUT the rest of the staff affordances (post-net summary)", () => {
    const onEdit = vi.fn();
    render(
      <ul>
        <RosterEntry
          entry={entry({
            key: "a",
            callsign: "W1AW",
            signalReport: "599",
            precedence: "priority",
            staying: "staying-for-comments",
          })}
          showSourceBadge
          showReport
          showPrecedence
          onEdit={onEdit}
        />
      </ul>,
    );
    expect(screen.getByText("599")).toBeInTheDocument();
    expect(screen.getByText("Priority").closest("[data-precedence]")).not.toBeNull();
    // NOT part of showReport/showPrecedence: the staying indicator and the Edit
    // affordance stay gated on the full staff `showSource`.
    expect(screen.queryByText("Staying")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /edit/i })).not.toBeInTheDocument();
  });

  it("renders the Heard relative-time column from addedAt", () => {
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", addedAt: "2026-07-16T00:00:00Z" })}
          now={new Date("2026-07-16T00:05:00Z")}
        />
      </ul>,
    );
    expect(screen.getByText("5m ago")).toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(
      <ul>
        <RosterEntry entry={entry({ key: "a", callsign: "W1AW" })} />
      </ul>,
    );
    await expectNoAxeViolations(container);
  });
});

describe("the way in", () => {
  it("renders the connection's LABEL on the row, never its id", () => {
    render(
      <RosterEntry
        entry={entry({
          key: "k1",
          callsign: "W1AW",
          via: { kind: "connection", connectionId: "conn-a" },
          viaLabel: "HF — 14.230 MHz",
        })}
      />,
    );
    const rendered = screen.getByText(/HF — 14.230 MHz/);
    expect(rendered).toBeTruthy();
    expect(rendered.textContent).toContain("HF — 14.230 MHz");
    expect(document.body.textContent).not.toContain("conn-a");
  });

  it("renders nothing at all when nobody recorded a way in", () => {
    const { container } = render(<RosterEntry entry={entry({ key: "k1", callsign: "W1AW", via: null, viaLabel: null })} />);
    expect(container.querySelector("[data-via]")).toBeNull();
  });

  it("says so in words when the way in cannot be resolved", () => {
    render(
      <RosterEntry
        entry={entry({
          key: "k1",
          callsign: "W1AW",
          via: { kind: "connection", connectionId: "gone" },
          viaLabel: UNRESOLVABLE_VIA_LABEL,
        })}
      />,
    );
    const cell = document.querySelector("[data-via]");
    expect(cell?.textContent).toContain(UNRESOLVABLE_VIA_LABEL);
    expect(cell?.textContent).not.toContain("gone");
  });
});

describe("the way-in correction annotation, end to end", () => {
  // Fold → buildRoster → render, rather than a hand-built `corrections` array:
  // the derivation (`viaKey`, `fromVia`/`toVia`), the render-time re-resolution
  // against the live connection set, and the annotation itself all shipped
  // separately, and only the whole path proves an operator sees LABELS.

  const uuid = (n: number): string =>
    `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

  function way(overrides: Record<string, unknown> = {}) {
    return {
      id: uuid(0x10),
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
    };
  }

  const echolink = way({
    id: uuid(0x11),
    position: 1,
    kind: "echolink",
    plannedFrequencyHz: null,
    band: null,
    mode: null,
    node: "N1CCK-R",
  });

  function correctedRow() {
    const store = createSessionStore();
    store.getState().seedFromSnapshot({
      id: uuid(1),
      definitionId: uuid(7),
      definitionVersion: 3,
      lifecycle: "live",
      connections: [way(), echolink],
      startedAt: "2026-07-16T00:00:00Z",
      closedAt: null,
      durationSeconds: null,
      latestSeq: 1,
      participantCount: 0,
      workingCheckInId: null,
      netNote: null,
      controlState: "active",
      roster: [
        {
          checkInId: uuid(42),
          callsign: "W1AW",
          addedAt: "2026-07-16T00:00:01Z",
          addedBy: uuid(200),
          source: "staff",
          staying: "in-and-out",
          precedence: "routine",
          worked: false,
          version: 1,
          via: { kind: "connection", connectionId: uuid(0x10) },
        },
      ],
      definition: {
        title: "Sunday Traffic Net",
        connections: [way()],
        netCategory: "traffic",
        netType: "open",
      },
    } as never);

    store.getState().applyEvent({
      kind: "checkin.updated",
      seq: 2,
      at: "2026-07-16T00:05:00Z",
      payload: {
        checkInId: uuid(42),
        callsign: "W1AW",
        staying: "in-and-out",
        precedence: "routine",
        version: 2,
        via: { kind: "connection", connectionId: uuid(0x11) },
      },
    } as never);

    return selectRoster(store.getState())[0];
  }

  it("names BOTH sides of the way-in correction, never a raw UUID", () => {
    // A REGRESSION PIN: the derivation, the re-resolution and the annotation
    // all shipped together, and this verifies the path rather than rebuilding
    // it.
    const { container } = render(
      <ul>
        <RosterEntry entry={correctedRow()} showSource />
      </ul>,
    );

    const note = container.querySelector('[data-correction="via"]');
    expect(note).not.toBeNull();
    expect(note?.textContent).toContain("EchoLink — N1CCK-R");
    expect(note?.textContent).toContain("HF — 14.230 MHz");
    expect(document.body.textContent).not.toContain(uuid(0x10));
    expect(document.body.textContent).not.toContain(uuid(0x11));
  });
});

describe("RosterEntry — the relaying station", () => {
  it("renders the relaying station as its own sub-line, distinct from the way in", () => {
    // The two facts are shown separately because they ARE separate: one says how
    // the traffic travelled, the other who passed it.
    render(
      <ul>
        <RosterEntry
          entry={entry({ key: "a", callsign: "W1AW", viaLabel: "EchoLink 12345", relayedBy: "W3REL" })}
        />
      </ul>,
    );
    const row = screen.getByRole("listitem");
    expect(row.querySelector("[data-via]")).not.toBeNull();
    const relay = row.querySelector("[data-relayed-by]");
    expect(relay).not.toBeNull();
    expect(relay?.textContent).toContain("W3REL");
  });

  it("renders NO relay sub-line for a station nobody relayed", () => {
    render(
      <ul>
        <RosterEntry entry={entry({ key: "a", callsign: "W1AW", relayedBy: null })} />
      </ul>,
    );
    expect(screen.getByRole("listitem").querySelector("[data-relayed-by]")).toBeNull();
  });

  it("names the relaying station in a correction annotation's field label", () => {
    // The label map is keyed on the fold's own token, `relayedBy`. A missing key
    // renders `undefined` at an operator, which is the failure this pins.
    render(
      <ul>
        <RosterEntry
          entry={entry({
            key: "a",
            callsign: "W1AW",
            corrections: [
              { field: "relayedBy", from: "W1ABC", to: "W1ABD", at: "2026-09-02T00:00:00Z" },
            ],
          })}
          showSource
        />
      </ul>,
    );
    const text = screen.getByRole("listitem").textContent ?? "";
    expect(text).not.toContain("undefined");
    expect(text).toContain("W1ABD");
  });
});
