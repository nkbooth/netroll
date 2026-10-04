// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { MAX_VIA_CHARS, ViaPicker, sendableVia } from "./ViaPicker";
import { UNRESOLVABLE_VIA_LABEL } from "../nets/connectionPresentation";
import type { NetConnection } from "../nets/netsApi";
import { expectNoAxeViolations } from "../../test/axe";

/**
 * The shared way-in picker — one component, two callers (the
 * quick-add's per-station override and the detail modal's editor). These assert
 * the SELECTION and the value it emits, never the copy around it.
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

/** Two repeaters on the SAME band — the only fixture that can tell
 * `connectionLabel` apart from `describeConnection(...).kindLabel`. Nothing
 * builds one by default, and without it the label mutation in T7
 * comes back green for the wrong reason. */
const sameBandRepeaters: readonly NetConnection[] = [
  connection({
    id: uuid(0x10),
    position: 0,
    kind: "repeater",
    plannedFrequencyHz: 146_940_000,
    band: "2m",
  }),
  connection({
    id: uuid(0x11),
    position: 1,
    kind: "repeater",
    plannedFrequencyHz: 147_210_000,
    band: "2m",
  }),
];

/** A net reached only over the internet — no tunable connection at all. */
const echolinkOnly: readonly NetConnection[] = [
  connection({
    id: uuid(0x20),
    kind: "echolink",
    plannedFrequencyHz: null,
    band: null,
    mode: null,
    node: "N1CCK-R",
  }),
];

function renderPicker(props: Partial<Parameters<typeof ViaPicker>[0]> = {}) {
  const onChange = vi.fn();
  const utils = render(
    <ViaPicker
      label="Came in on"
      unsetLabel="Not recorded"
      connections={[connection()]}
      value={null}
      onChange={onChange}
      {...props}
    />,
  );
  return { ...utils, onChange };
}

describe("ViaPicker — the options it offers", () => {
  it("labels each option with connectionLabel, so two same-band repeaters differ", () => {
    // `describeConnection(...).kindLabel` renders BOTH of these as "Repeater",
    // which is what `FrequencyControl`'s option line does today. The
    // identifying fact is what makes the choice a real one.
    renderPicker({ connections: sameBandRepeaters });

    const group = screen.getByRole("radiogroup", { name: "Came in on" });
    const labels = within(group)
      .getAllByRole("radio")
      .map((radio) => radio.textContent ?? "");
    expect(labels).toContain("Repeater — 146.940 MHz");
    expect(labels).toContain("Repeater — 147.210 MHz");
  });

  it("offers EVERY connection, not just the tunable ones", () => {
    // An EchoLink-only net has no RF connection at all, and its operator still
    // needs to record which way a station came in.
    renderPicker({ connections: echolinkOnly });

    const group = screen.getByRole("radiogroup", { name: "Came in on" });
    const labels = within(group)
      .getAllByRole("radio")
      .map((radio) => radio.textContent ?? "");
    expect(labels).toContain("EchoLink — N1CCK-R");
  });

  it("preselects the value it was handed and nothing else", () => {
    renderPicker({
      connections: sameBandRepeaters,
      value: { kind: "connection", connectionId: uuid(0x11) },
    });

    const group = screen.getByRole("radiogroup", { name: "Came in on" });
    const checked = within(group)
      .getAllByRole("radio")
      .filter((radio) => radio.getAttribute("aria-checked") === "true")
      .map((radio) => radio.textContent ?? "");
    expect(checked).toEqual(["Repeater — 147.210 MHz"]);
  });

  it("preselects the unset option when nothing was recorded", () => {
    renderPicker();

    const group = screen.getByRole("radiogroup", { name: "Came in on" });
    const checked = within(group)
      .getAllByRole("radio")
      .filter((radio) => radio.getAttribute("aria-checked") === "true")
      .map((radio) => radio.textContent ?? "");
    expect(checked).toEqual(["Not recorded"]);
  });

  it("names an unresolvable value in words and does not OFFER it", () => {
    // Near-unreachable (the session snapshot is frozen) — but the control must
    // not be the one place that prints a raw id, and it must not let the
    // operator re-pick a way in the net no longer lists.
    renderPicker({
      connections: sameBandRepeaters,
      value: { kind: "connection", connectionId: uuid(0x99) },
    });

    const group = screen.getByRole("radiogroup", { name: "Came in on" });
    const radios = within(group).getAllByRole("radio");
    expect(radios.map((r) => r.textContent)).not.toContain(UNRESOLVABLE_VIA_LABEL);
    expect(screen.getByText(UNRESOLVABLE_VIA_LABEL)).toBeInTheDocument();
    expect(document.body.textContent).not.toContain(uuid(0x99));
  });
});

describe("ViaPicker — what selecting emits", () => {
  it("emits the connection's id, never its label", async () => {
    const user = userEvent.setup();
    const { onChange } = renderPicker({ connections: sameBandRepeaters });

    await user.click(screen.getByRole("radio", { name: "Repeater — 147.210 MHz" }));

    expect(onChange).toHaveBeenCalledWith({
      kind: "connection",
      connectionId: uuid(0x11),
    });
  });

  it("emits null when the operator returns to the unset option", async () => {
    const user = userEvent.setup();
    const { onChange } = renderPicker({
      value: { kind: "connection", connectionId: uuid(1) },
    });

    await user.click(screen.getByRole("radio", { name: "Not recorded" }));

    expect(onChange).toHaveBeenCalledWith(null);
  });
});

describe("ViaPicker — the free-text path", () => {
  it("bounds the free-text box at MAX_VIA_CHARS so 65 characters cannot be typed", async () => {
    const user = userEvent.setup();
    const { onChange } = renderPicker();

    await user.click(screen.getByRole("radio", { name: /other/i }));
    const box = screen.getByRole("textbox", { name: /way in/i });
    await user.type(box, "x".repeat(70));

    expect((box as HTMLInputElement).value).toHaveLength(64);
    expect(onChange).toHaveBeenLastCalledWith({
      kind: "unlisted",
      text: "x".repeat(64),
    });
  });

  it("emits NOTHING RECORDED for a blank-after-trim free text, never an empty value", async () => {
    // `parse_via_text` treats blank-after-trim as an ERROR, not as `None` — the
    // one place this field departs from every other optional text field in the
    // tree. A `{"kind":"unlisted","text":""}` on the wire 400s and takes the
    // whole check-in with it.
    const user = userEvent.setup();
    const { onChange } = renderPicker();

    await user.click(screen.getByRole("radio", { name: /other/i }));
    await user.type(screen.getByRole("textbox", { name: /way in/i }), "   ");

    expect(onChange).toHaveBeenLastCalledWith(null);
  });

  it("keeps the free-text option selected while its box is still empty", async () => {
    // Otherwise the selection snaps back to "not recorded" under the operator
    // between the tap and the first keystroke.
    const user = userEvent.setup();
    renderPicker();

    await user.click(screen.getByRole("radio", { name: /other/i }));

    expect(screen.getByRole("radio", { name: /other/i })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("carries a single-line, length-bounded box rather than a prose field", async () => {
    // Renamed 2026-08-31 (review). This asserts what it can see: an `<input>`
    // bounded at the server's own limit. The stronger claim its old name made
    // — "a newline can never reach the wire" — is enforced by the platform's
    // value-sanitization algorithm for `input[type=text]`, which jsdom does not
    // model, so a test written to that name would have passed on jsdom's
    // leniency rather than on the guarantee. The newline case is tested where
    // it is actually decided, in `sendableVia` below.
    //
    // The bound is asserted BEHAVIOURALLY: the box no longer
    // carries a native `maxlength` (that attribute counts UTF-16 units of the
    // untrimmed value, which is not the server's bound), so the evidence is
    // that a 65th code point cannot be committed.
    const user = userEvent.setup();
    const { onChange } = renderPicker();
    await user.click(screen.getByRole("radio", { name: /other/i }));
    const box = screen.getByRole("textbox", { name: /way in/i });
    expect(box.tagName).toBe("INPUT");

    await user.click(box);
    await user.paste("x".repeat(65));

    const emitted = onChange.mock.calls.at(-1)?.[0] as { text: string } | null;
    expect([...(emitted?.text ?? "")]).toHaveLength(64);
  });

  it("takes a pasted control character out of the box before it can be committed", async () => {
    // The 400 must be unreachable BY CONSTRUCTION. `input[type=text]`
    // sanitizes CR/LF for free but leaves a TAB alone, and a tab arrives by
    // paste from a spreadsheet cell or a copied callsign list. The server
    // refuses every control character, and because `via` rides the callsign's
    // own POST the rejection takes the station with it.
    const user = userEvent.setup();
    const { onChange } = renderPicker();
    await user.click(screen.getByRole("radio", { name: /other/i }));
    const box = screen.getByRole("textbox", { name: /way in/i });

    await user.click(box);
    await user.paste("Bob's\thotspot");

    // Asserted by code point rather than by a regex literal: a character class
    // spelling out the control range trips oxlint's `no-control-regex`, and the
    // accepted warning baseline is not the place to spend a lint exemption.
    const carriesControlChar = (text: string): boolean =>
      [...text].some((character) => {
        const code = character.codePointAt(0) ?? 0;
        return code < 0x20 || (code >= 0x7f && code <= 0x9f);
      });

    expect(carriesControlChar((box as HTMLInputElement).value)).toBe(false);
    const emitted = onChange.mock.calls.at(-1)?.[0] as { text: string } | null;
    expect(carriesControlChar(emitted?.text ?? "")).toBe(false);
  });
});

describe("ViaPicker — the box's bound is the server's bound", () => {
  // `parse_bounded_text` trims, then counts `chars()` — Unicode scalar values
  // of the TRIMMED value. The native `maxLength` attribute counted UTF-16 units
  // of the RAW value, so the two bounds disagreed in both directions: an
  // astral-plane character cost two, and a leading space cost one. The primary
  // assertion in every test here is on `onChange`'s argument, because the
  // emitted `via` is what reaches the wire; the box's rendered value is a
  // sibling surface, asserted only where it is the sole surface a defect can
  // reach.
  const lastEmittedText = (onChange: ReturnType<typeof vi.fn>): string => {
    const emitted = onChange.mock.calls.at(-1)?.[0] as { text: string } | null;
    return emitted?.text ?? "";
  };

  it("admits MAX_VIA_CHARS astral-plane characters, counted as code points not UTF-16 units", async () => {
    const user = userEvent.setup();
    const { onChange } = renderPicker();
    await user.click(screen.getByRole("radio", { name: /other/i }));

    await user.type(screen.getByRole("textbox", { name: /way in/i }), "\u{1F6F0}".repeat(64));

    // Spread length, never `.length`: 64 UTF-16 units of this string is 32 code
    // points, and an assertion on `.length` would pass for the wrong reason.
    expect([...lastEmittedText(onChange)]).toHaveLength(64);
  });

  it("clamps an over-long astral-plane paste to MAX_VIA_CHARS whole code points, never a split surrogate", async () => {
    // Every in-bound astral input takes the clamp's identity fast path, so the
    // truncating branch was only ever exercised by ASCII. Slicing the string
    // rather than its code points there cuts a 65-emoji paste to 32 whole
    // characters and one unpaired high surrogate, which `JSON.stringify` emits
    // as a lone `\ud83d` escape and the server rejects as a malformed body —
    // losing the whole check-in, not just the way in.
    const user = userEvent.setup();
    const { onChange } = renderPicker();
    await user.click(screen.getByRole("radio", { name: /other/i }));
    const box = screen.getByRole("textbox", { name: /way in/i });

    await user.click(box);
    await user.paste("\u{1F6F0}".repeat(MAX_VIA_CHARS + 1));

    expect([...lastEmittedText(onChange)]).toHaveLength(MAX_VIA_CHARS);
  });

  it("does not charge leading whitespace against the bound, because the server trims first", async () => {
    const user = userEvent.setup();
    const { onChange } = renderPicker();
    await user.click(screen.getByRole("radio", { name: /other/i }));
    const box = screen.getByRole("textbox", { name: /way in/i });

    await user.type(box, "  " + "x".repeat(64));

    expect(lastEmittedText(onChange)).toBe("x".repeat(64));
    // The rendered value IS asserted here, against this block's own rule,
    // because it is the only surface on which the bound's trim is observable:
    // the clamp's `trimStart()` recovers leading padding and the emit's own
    // `trim()` discards it, so the emitted text is identical whether the bound
    // counted the raw or the trimmed value. Only the box can tell them apart.
    expect(box).toHaveValue("  " + "x".repeat(64));
  });

  it("refuses a keystroke into a FULL box rather than destroying the character the operator never touched", async () => {
    // The native attribute truncated the INSERTION and left the existing value
    // alone. Clamping the RESULT instead conflates the two: with 64 code points
    // held and the caret mid-string, one keystroke makes 65, and slicing to 64
    // cuts off the trailing character — the one the operator was not editing.
    const user = userEvent.setup();
    const full = "A".repeat(63) + "Z";
    const { onChange } = renderPicker({ value: { kind: "unlisted", text: full } });
    const box = screen.getByRole("textbox", { name: /way in/i }) as HTMLInputElement;

    box.focus();
    box.setSelectionRange(10, 10);
    await user.keyboard("Q");

    expect([...lastEmittedText(onChange)].at(-1)).toBe("Z");
  });

  it("charges a LEADING U+FEFF nothing against the bound, and does not send it", async () => {
    // U+FEFF is the ONE character JS `trim()` removes and Rust `str::trim` does
    // not (it left the White_Space property in Unicode 4.0.1; ECMAScript still
    // lists <ZWNBSP>). A pasted BOM is not a control and not a bidi mark, so
    // the sanitiser does not see it; at the EDGES the emit's trim does, and the
    // 64 characters behind it still fit. That is the whole of the claim — an
    // interior U+FEFF is a different case, pinned below.
    const user = userEvent.setup();
    const { onChange } = renderPicker();
    await user.click(screen.getByRole("radio", { name: /other/i }));
    const box = screen.getByRole("textbox", { name: /way in/i });

    await user.click(box);
    await user.paste("\uFEFF" + "x".repeat(64));

    expect(lastEmittedText(onChange)).toBe("x".repeat(64));
  });

  it("passes an INTERIOR U+FEFF through to the emit unchanged", async () => {
    // No U+FEFF fold. Interior, the mark is `Cf` — not
    // `Cc`, not a bidi control — so `withoutIllegalChars` leaves it, the clamp
    // counts it as one code point exactly as the server's `chars()` does, and
    // `parse_bounded_text` accepts it. Both sides count it identically, so it
    // is not a bound disagreement; this pins that the client does not invent
    // a stricter rule than the server has.
    const user = userEvent.setup();
    const { onChange } = renderPicker();
    await user.click(screen.getByRole("radio", { name: /other/i }));
    const box = screen.getByRole("textbox", { name: /way in/i });

    await user.click(box);
    await user.paste("Bob\uFEFFhotspot");

    expect(lastEmittedText(onChange)).toBe("Bob\uFEFFhotspot");
  });
});

describe("ViaPicker — the keyboard and the accessibility floor", () => {
  it("is ONE tab stop: a roving tabindex, not one stop per option", () => {
    renderPicker({ connections: sameBandRepeaters });

    const group = screen.getByRole("radiogroup", { name: "Came in on" });
    const radios = within(group).getAllByRole("radio");
    const reachable = radios.filter((radio) => radio.tabIndex === 0);
    expect(reachable).toHaveLength(1);
    expect(reachable[0].getAttribute("aria-checked")).toBe("true");
  });

  it("moves the selection with the arrow keys once the group has focus", async () => {
    const user = userEvent.setup();
    const { onChange } = renderPicker({
      connections: sameBandRepeaters,
      value: { kind: "connection", connectionId: uuid(0x10) },
    });

    screen.getByRole("radio", { name: "Repeater — 146.940 MHz" }).focus();
    await user.keyboard("{ArrowRight}");

    expect(onChange).toHaveBeenLastCalledWith({
      kind: "connection",
      connectionId: uuid(0x11),
    });
  });

  it("activates the focused option with Space", async () => {
    const user = userEvent.setup();
    const { onChange } = renderPicker({ connections: sameBandRepeaters });

    screen.getByRole("radio", { name: "Repeater — 146.940 MHz" }).focus();
    await user.keyboard(" ");

    expect(onChange).toHaveBeenLastCalledWith({
      kind: "connection",
      connectionId: uuid(0x10),
    });
  });

  it("conveys the selection with a shape and a label, never colour alone", () => {
    // An operator on a phone in the dark. The selected chip
    // carries a non-colour affordance its unselected siblings do not.
    renderPicker({
      connections: sameBandRepeaters,
      value: { kind: "connection", connectionId: uuid(0x10) },
    });

    const group = screen.getByRole("radiogroup", { name: "Came in on" });
    const marked = within(group)
      .getAllByRole("radio")
      .filter((radio) => radio.querySelector("svg") !== null);
    expect(marked).toHaveLength(1);
    expect(marked[0]).toHaveAttribute("aria-checked", "true");
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = renderPicker({ connections: sameBandRepeaters });
    await expectNoAxeViolations(container);
  });
});

describe("sendableVia — what is actually safe to POST", () => {
  it("passes a connection this session still lists", () => {
    expect(sendableVia({ kind: "connection", connectionId: uuid(1) }, [connection()])).toEqual({
      kind: "connection",
      connectionId: uuid(1),
    });
  });

  it("trims free text, so the optimistic row and the server's echo agree", () => {
    // The server trims before persisting (`parse_bounded_text`), so untrimmed
    // text renders padded in the pending row and then visibly reflows when the
    // authoritative echo lands. Every other free-text field in this feature —
    // grid, notes, publicNote, name, location — trims before assignment.
    expect(sendableVia({ kind: "unlisted", text: "  Bob's hotspot  " }, [])).toEqual({
      kind: "unlisted",
      text: "Bob's hotspot",
    });
  });

  it("refuses free text carrying a control character rather than losing the station", () => {
    // The worst case. The server answers a control character with 400
    // `/errors/via-invalid`, and `via` rides the callsign's POST, so sending it
    // fails the whole check-in. Dropping the way in logs the station; sending
    // it loses the station AND the way in. This is the belt behind the box's
    // own sanitising — reaching it at all is a defect upstream.
    expect(sendableVia({ kind: "unlisted", text: "Bob's\thotspot" }, [])).toBeUndefined();
    expect(sendableVia({ kind: "unlisted", text: "Bob's\nhotspot" }, [])).toBeUndefined();
  });

  it("refuses free text carrying a bidi control, which the server also rejects", () => {
    // `is_bidi_control` — U+200E/200F, U+202A..202E, U+2066..2069. Not a
    // control character by Unicode category, so a guard written only against
    // `Cc` would let it through to the same 400.
    expect(sendableVia({ kind: "unlisted", text: "Bob\u202Ehotspot" }, [])).toBeUndefined();
  });

  it("refuses free text over the server's bound rather than losing the station", () => {
    // `parse_bounded_text` answers a 65th code point with 400 `/errors/via-invalid`
    // (`ProfileError::TooLong`), and `via` rides the callsign's POST. Counted the
    // server's way — code points of the trimmed value — so 64 astral characters
    // pass while 65 ASCII ones do not, and padding costs nothing.
    expect(sendableVia({ kind: "unlisted", text: "x".repeat(65) }, [])).toBeUndefined();
    expect(sendableVia({ kind: "unlisted", text: "  " + "\u{1F6F0}".repeat(64) + "  " }, [])).toEqual({
      kind: "unlisted",
      text: "\u{1F6F0}".repeat(64),
    });
  });

  it("refuses to send a connection the session cannot resolve", () => {
    // The alternative is posting a dangling reference the server answers 404
    // for — which fails the whole add and loses the station.
    expect(
      sendableVia({ kind: "connection", connectionId: uuid(0x99) }, [connection()]),
    ).toBeUndefined();
  });

  it("passes free text through untouched — it resolves against nothing", () => {
    expect(sendableVia({ kind: "unlisted", text: "Bob's hotspot" }, [])).toEqual({
      kind: "unlisted",
      text: "Bob's hotspot",
    });
  });

  it("sends NO KEY when nothing was recorded", () => {
    expect(sendableVia(null, [connection()])).toBeUndefined();
  });
});

describe("ViaPicker — a value that changes UNDER the control (review 2026-08-31)", () => {
  it("shows the free text a stamp brought in after mount, without a remount", () => {
    // `otherMode`/`freeText` were mount-once `useState`, while `value` is a
    // live prop: the quick-add's picker only remounts after a SUCCESSFUL
    // commit, so a stamp set in the toolbar before the first check-in ticked
    // "Other…" while the box that holds the text never rendered at all.
    const { rerender, onChange } = renderPicker({ value: null });
    expect(screen.queryByRole("textbox", { name: /way in/i })).toBeNull();

    rerender(
      <ViaPicker
        label="Came in on"
        unsetLabel="Not recorded"
        connections={[connection()]}
        value={{ kind: "unlisted", text: "Bob's hotspot" }}
        onChange={onChange}
      />,
    );

    const box = screen.getByRole("textbox", { name: /way in/i });
    expect((box as HTMLInputElement).value).toBe("Bob's hotspot");
  });

  it("does not clear the way in when the already-selected Other chip is tapped", async () => {
    // The data-loss half. The value must arrive AFTER mount to reach it: with
    // free text present at mount the `useState` initializer seeds `freeText`
    // and the tap re-emits it correctly, which is why this pin has to rerender
    // first. `emitForKey(OTHER_KEY)` then read the STALE local `freeText`
    // (still ""), so one tap on a chip that already LOOKED selected emitted
    // `null` — that station recorded no way in while the toolbar still named
    // one.
    const user = userEvent.setup();
    const { rerender, onChange } = renderPicker({ value: null });

    rerender(
      <ViaPicker
        label="Came in on"
        unsetLabel="Not recorded"
        connections={[connection()]}
        value={{ kind: "unlisted", text: "Bob's hotspot" }}
        onChange={onChange}
      />,
    );
    expect(screen.getByRole("radio", { name: /other/i })).toHaveAttribute(
      "aria-checked",
      "true",
    );

    await user.click(screen.getByRole("radio", { name: /other/i }));

    expect(onChange).not.toHaveBeenCalledWith(null);
    expect(onChange).toHaveBeenLastCalledWith({ kind: "unlisted", text: "Bob's hotspot" });
  });

  it("keeps the operator's own draft when the stamp changes under them", async () => {
    // The other direction: following the prop must not yank away text the
    // operator is in the middle of typing as their per-station override.
    const user = userEvent.setup();
    const { rerender, onChange } = renderPicker({ value: null });
    await user.click(screen.getByRole("radio", { name: /other/i }));
    await user.type(screen.getByRole("textbox", { name: /way in/i }), "Kitchen HT");

    rerender(
      <ViaPicker
        label="Came in on"
        unsetLabel="Not recorded"
        connections={[connection()]}
        value={{ kind: "unlisted", text: "Kitchen HT" }}
        onChange={onChange}
      />,
    );

    expect((screen.getByRole("textbox", { name: /way in/i }) as HTMLInputElement).value).toBe(
      "Kitchen HT",
    );
  });
});
