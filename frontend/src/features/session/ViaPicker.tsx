// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useRef, useState } from "react";
import type { CSSProperties, ReactElement, ReactNode } from "react";

import {
  UNRESOLVABLE_VIA_LABEL,
  connectionLabel,
  type ViaWire,
} from "../nets/connectionPresentation";
import type { NetConnection } from "../nets/netsApi";
import { tokens } from "../../ui/tokens/tokens";

/**
 * The way-in picker — one control, two callers: the
 * quick-add's one-tap per-station override and the detail modal's editor.
 *
 * Extracted with only two call sites, deliberately against the three-strike
 * rule, because both are written in the same story and the two things that must
 * not drift between them are the two this project has already been burned by:
 * WHICH label function names an option (`connectionLabel`, never
 * `describeConnection(...).kindLabel` — that one renders two same-band repeaters
 * identically, which `FrequencyControl` still does), and HOW free text is
 * bounded (64 characters, single line, and blank-after-trim is NOT a value).
 * Writing those twice and merging later means shipping the drift first.
 *
 * Purely presentational: it holds no stamp, sends nothing, and knows no session.
 * The tri-state dirty tracking the modal needs is the modal's own concern.
 *
 * **Its free-text box is a DRAFT OVER the `value` prop, not a copy of it**: the
 * local state starts as `null` meaning "follow the prop", and holds a string
 * only once the operator has typed. So a `value` that changes UNDER the control
 * — the session stamp being set in the toolbar after the quick-add mounted — is
 * shown immediately, while text the operator is part-way through typing is never
 * yanked out from under them. Derived, not synced: there is still no effect here
 * writing the prop back into state, which is the bug this shape avoids rather
 * than manages.
 *
 * A caller that needs the draft DISCARDED (the quick-add, after every commit)
 * remounts the control with a changed `key` rather than reaching in — the React
 * reset-by-key idiom. Before the review of 2026-08-31 that key was the ONLY
 * thing keeping the box in step with the prop, and it only moved on a successful
 * commit, so a stamp set before the operator's first check-in ticked "Other…"
 * over a box that never rendered — and one tap on that already-ticked chip
 * emitted `null`, recording no way in at all.
 */

/**
 * The bound the server enforces on a free-text way in
 * (`netroll-domain`'s `MAX_VIA_CHARS`).
 *
 * A HAND-WRITTEN TWIN, in the sense `UNRESOLVABLE_VIA_LABEL` and
 * `isReservedConnectionLabel` already are: mirrored here so the box cannot be
 * over-typed, never shared, and a disagreement between the two is a defect in
 * whichever was edited alone. The server stays the authority — this is UX only,
 * and its whole job is that the operator never DISCOVERS the bound by
 * losing a station to a 400 at 20:07.
 */
export const MAX_VIA_CHARS = 64;

/**
 * Every character the server refuses in a single-line free-text field: the
 * Unicode `Cc` controls, plus the bidi marks and overrides.
 *
 * ANOTHER HAND-WRITTEN TWIN, of `parse_bounded_text`'s
 * `c.is_control() || is_bidi_control(c)` — mirrored, never shared, and a
 * disagreement is a defect in whichever was edited alone.
 *
 * The box needs this because `input type="text"` sanitises CR and LF for free
 * (the HTML value-sanitization algorithm) and NOTHING else: a TAB survives a
 * paste from a spreadsheet cell, and a bidi mark survives a paste from a web
 * page. The server answers either with 400 `/errors/via-invalid`, and `via`
 * rides the callsign's own POST — so a character the operator cannot even see
 * takes the whole station with it. That has to be unreachable by
 * construction, which is this.
 */
const ILLEGAL_VIA_CLASS =
  "[\\u0000-\\u001F\\u007F-\\u009F\\u200E\\u200F\\u202A-\\u202E\\u2066-\\u2069]";
const ILLEGAL_VIA_ANY = new RegExp(ILLEGAL_VIA_CLASS);
const ILLEGAL_VIA_ALL = new RegExp(ILLEGAL_VIA_CLASS, "g");

/** Replaces each refused character with a space rather than deleting it: a tab
 * pasted from a spreadsheet sat BETWEEN two words, and closing that gap would
 * silently rewrite "Bob\u2019s hotspot" into one. Trimming at the edges is the
 * emit path's job. */
function withoutIllegalChars(text: string): string {
  return text.replace(ILLEGAL_VIA_ALL, " ");
}

/**
 * True when `text` is inside the bound the SERVER enforces, counted the way the
 * server counts it: Unicode scalar values of the TRIMMED value.
 *
 * `[...text]` iterates code points where `.length` counts UTF-16 units, which is
 * the half of the mismatch an astral-plane callsign falls down; trimming first is
 * the other half. Both mirror `parse_bounded_text`, which trims and then counts
 * `chars()`. The native `maxLength` attribute did neither — it bounded the raw
 * value in UTF-16 units — so the box was stricter than the server in both ways.
 *
 * JS `trim()` strips a superset of what `str::trim` does (U+FEFF, and U+0085
 * — though the sanitiser folds that one to a space first). Counting with a
 * trim the server does not share is harmless ONLY because every emit path
 * sends the JS-TRIMMED value, never the raw draft: the server's own trim can
 * then only shrink what arrives further, so anything this admits the server
 * admits too. An emit that sent the untrimmed draft, or a clamp that stopped
 * trimming, would silently make the client the looser side — the disagreement
 * identified and deliberately not folded against, on this invariant.
 */
function withinViaBound(text: string): boolean {
  return [...text.trim()].length <= MAX_VIA_CHARS;
}

/** Keeps at most MAX_VIA_CHARS code points, measured the server's way. Leading
 * whitespace is stripped from the box only when the value is actually over the
 * bound; under it the padding stays where the operator put it. (An INTERIOR
 * space is not free — the server trims ends only, so it is one `char` against
 * the 64 on both sides.) Clamps rather than refuses because that is what the
 * native attribute did with an over-long paste — refusing would silently
 * discard a whole pasted name. */
function clampToViaBound(text: string): string {
  if (withinViaBound(text)) {
    return text;
  }
  return [...text.trimStart()].slice(0, MAX_VIA_CHARS).join("");
}

/** The box's next draft after an edit, or the current one if the edit is
 * refused. The native attribute truncated the INSERTION and left the existing
 * value alone; clamping the RESULT conflates the two, so with 64 code points
 * held and the caret mid-string one keystroke made 65 and slicing to 64
 * destroyed the trailing character — the one the operator was not editing.
 * Refusing the edit when there is no room reproduces the attribute's half that
 * matters (the held value survives intact); clamping only when there IS room
 * is what still lands an over-long paste. It does NOT
 * reproduce the attribute's caret: the DOM node already holds the insertion
 * when this runs, so React writes the refused draft back over it and the caret
 * lands at the end (measured on jsdom 2026-09-05: `selectionStart` 64 after a
 * refused keystroke at offset 10). */
function nextViaDraft(current: string, edited: string): string {
  if (withinViaBound(edited)) {
    return edited;
  }
  if ([...current.trim()].length === MAX_VIA_CHARS) {
    return current;
  }
  return clampToViaBound(edited);
}

/** The sentinel option key for "nobody recorded a way in". */
const UNSET_KEY = "none";

/** The sentinel option key for the free-text path. */
const OTHER_KEY = "other";

const groupStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
  // The group wraps within its own container rather than widening the row it
  // sits in — a net with several connections must not push the quick-add wider
  // than a ~340px phone.
  minWidth: 0,
  maxWidth: "100%",
};

const chipRowStyle: CSSProperties = {
  display: "flex",
  flexWrap: "wrap",
  gap: "var(--space-1)",
  minWidth: 0,
  maxWidth: "100%",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  color: "var(--text-muted)",
  textTransform: "uppercase",
  letterSpacing: "0.04em",
};

const noteStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--sync-text)",
};

const freeTextStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-2)",
  background: "var(--surface)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  minWidth: 0,
  maxWidth: "100%",
};

/** A tick — the selected chip's NON-COLOUR affordance (an
 * operator reads this on a phone in the dark, so colour is never the only
 * signal). Deliberately not a coral fill: coral is reserved for `w`/working. */
const checkIcon: ReactNode = (
  <svg
    width="10"
    height="10"
    viewBox="0 0 10 10"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    aria-hidden="true"
  >
    <path d="M1.5 5.5 L4 8 L8.5 2.5" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

function chipStyle(selected: boolean): CSSProperties {
  return {
    display: "inline-flex",
    alignItems: "center",
    gap: "var(--space-1)",
    padding: "var(--space-1) var(--space-2)",
    background: selected ? "var(--surface-2)" : "transparent",
    color: selected ? "var(--text)" : "var(--text-muted)",
    border: `1px solid ${selected ? "var(--accent)" : "var(--border)"}`,
    borderRadius: "var(--rounded-full)",
    font: "inherit",
    fontSize: tokens.typography.labelCaps.fontSize,
    fontWeight: selected ? 700 : 600,
    cursor: "pointer",
  };
}

/**
 * The `via` actually safe to put on the wire, or `undefined` for "send no key".
 *
 * The ONE place that decides this, for both the add and the edit path. Free
 * text comes back TRIMMED — the server trims before persisting, so sending the
 * padded form makes the optimistic row and the authoritative echo disagree
 * about the same string, and the operator watches their own row reflow.
 *
 * Four values are refused, and every one of them is refused because SENDING it
 * is worse than dropping it: `via` rides the callsign's own POST, so a rejected
 * way in fails the whole check-in and loses the station too.
 *
 * - nothing recorded;
 * - free text that is blank after trimming, or carries a character the server
 * will not take (400 `/errors/via-invalid`) — the belt behind the box's own
 * sanitising, which should mean this arm is unreachable from the UI;
 * - free text over `MAX_VIA_CHARS` code points once trimmed
 * (`ProfileError::TooLong`, the same 400) — the belt behind the box's clamp,
 * and unreachable from the UI for the same reason;
 * - a connection this session's frozen snapshot no longer lists (404
 * `/errors/net-connection-not-found`).
 */
export function sendableVia(
  via: ViaWire | null,
  connections: readonly NetConnection[],
): ViaWire | undefined {
  if (via === null) {
    return undefined;
  }
  if (via.kind === "unlisted") {
    const text = via.text.trim();
    if (text === "" || ILLEGAL_VIA_ANY.test(text) || !withinViaBound(text)) {
      return undefined;
    }
    return { kind: "unlisted", text };
  }
  return connections.some((c) => c.id === via.connectionId) ? via : undefined;
}

export interface ViaPickerProps {
  /** Names the group to assistive tech, and heads it on screen. */
  readonly label: string;
  /** What the "nobody recorded one" option is called — the quick-add and the
   * modal mean subtly different things by it, so neither wording is baked in. */
  readonly unsetLabel: string;
  /** The session's frozen connection set. EVERY entry is offered — not
   * `FrequencyControl`'s tunable filter, which would leave an EchoLink-only
   * net's operator with no option at all. */
  readonly connections: readonly NetConnection[];
  /** The way in currently in force, or `null` for none. */
  readonly value: ViaWire | null;
  /** Fires with the operator's choice. `null` means NOT RECORDED — including
   * for a free-text box that is blank after trimming, which must never reach
   * the wire as `{"kind":"unlisted","text":""}` (the server treats that as an
   * error, not as "absent"). */
  readonly onChange: (via: ViaWire | null) => void;
}

/** One-tap way-in chips plus the free-text path, as a single tab stop. */
export function ViaPicker({
  label,
  unsetLabel,
  connections,
  value,
  onChange,
}: ViaPickerProps): ReactElement {
  // The free-text box is a DRAFT OVER the prop: `null` means "follow `value`",
  // a string means the operator has typed and owns the box until it remounts.
  // Both halves matter — following the prop is what shows a stamp set after
  // this mounted; holding the draft is what stops a stamp change mid-typing
  // from yanking a part-typed override away.
  const [draft, setDraft] = useState<string | null>(null);
  const [otherTapped, setOtherTapped] = useState(false);
  const inForceText = value !== null && value.kind === "unlisted" ? value.text : "";
  const freeText = draft ?? inForceText;
  // Derived, never synced. `otherTapped` covers the one state the prop cannot
  // express: the operator has chosen free text but not yet typed any, so
  // `value` is legitimately `null` and the box must still be on screen.
  const otherMode = otherTapped || (value !== null && value.kind === "unlisted");
  const chipRefs = useRef<Map<string, HTMLButtonElement>>(new Map());

  const optionKeys = [UNSET_KEY, ...connections.map((c) => c.id), OTHER_KEY];
  const selectedKey = otherMode
    ? OTHER_KEY
    : value === null
      ? UNSET_KEY
      : value.kind === "unlisted"
        ? OTHER_KEY
        : value.connectionId;
  // A `via` naming a connection the session no longer lists selects NOTHING —
  // it is not offered back as a choice, and the note below says so in words
  // rather than printing the id.
  const unresolvable = optionKeys.includes(selectedKey) ? null : UNRESOLVABLE_VIA_LABEL;
  // The single tab stop lands on the selection; with nothing selected it falls
  // back to the first option, so the group is never unreachable by keyboard.
  const tabStopKey = unresolvable === null ? selectedKey : UNSET_KEY;

  const emitForKey = (key: string): void => {
    if (key === OTHER_KEY) {
      setOtherTapped(true);
      // Reads the EFFECTIVE text, so tapping a chip that already looks selected
      // re-states the way in rather than clearing it. Deliberately does not pin
      // a draft: with none held, the box keeps following the prop until the
      // operator actually types.
      const text = freeText.trim();
      onChange(text === "" ? null : { kind: "unlisted", text });
      return;
    }
    setOtherTapped(false);
    onChange(key === UNSET_KEY ? null : { kind: "connection", connectionId: key });
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>, key: string): void => {
    const step =
      event.key === "ArrowRight" || event.key === "ArrowDown"
        ? 1
        : event.key === "ArrowLeft" || event.key === "ArrowUp"
          ? -1
          : 0;
    if (step === 0) {
      return;
    }
    event.preventDefault();
    const current = optionKeys.indexOf(key);
    const nextKey = optionKeys[(current + step + optionKeys.length) % optionKeys.length];
    emitForKey(nextKey);
    chipRefs.current.get(nextKey)?.focus();
  };

  const chip = (key: string, text: string): ReactElement => {
    const selected = key === selectedKey;
    return (
      <button
        key={key}
        type="button"
        role="radio"
        aria-checked={selected}
        tabIndex={key === tabStopKey ? 0 : -1}
        ref={(node) => {
          if (node === null) {
            chipRefs.current.delete(key);
          } else {
            chipRefs.current.set(key, node);
          }
        }}
        onClick={() => emitForKey(key)}
        onKeyDown={(event) => onKeyDown(event, key)}
        style={chipStyle(selected)}
      >
        {selected && checkIcon}
        <span>{text}</span>
      </button>
    );
  };

  return (
    <div style={groupStyle}>
      <span style={labelStyle}>{label}</span>
      <div role="radiogroup" aria-label={label} style={chipRowStyle}>
        {chip(UNSET_KEY, unsetLabel)}
        {connections.map((connection) => chip(connection.id, connectionLabel(connection)))}
        {chip(OTHER_KEY, "Other…")}
      </div>
      {unresolvable !== null && <span style={noteStyle}>{unresolvable}</span>}
      {otherMode && (
        <label style={groupStyle}>
          <span style={labelStyle}>Another way in</span>
          <input
            type="text"
            autoComplete="off"
            value={freeText}
            onChange={(event) => {
              // Sanitised BEFORE it is ever held, so the box itself cannot
              // carry a character the server refuses — a pasted tab or bidi
              // mark never becomes a value the operator could commit. Bounded
              // AFTER sanitising, because folding a character to a space can
              // change the trimmed length the bound measures. No native
              // `maxLength`: that counts UTF-16 units of the untrimmed value,
              // which is not the bound the server enforces.
              const typed = nextViaDraft(freeText, withoutIllegalChars(event.target.value));
              setDraft(typed);
              // Blank after trim is NOT an empty value — the server reads
              // `{"kind":"unlisted","text":""}` as an error and 400s the whole
              // check-in. The way to say "not recorded" is to send no key. The
              // emitted text is trimmed while the box keeps what was typed, so
              // a space part-way through a word costs nothing.
              const text = typed.trim();
              onChange(text === "" ? null : { kind: "unlisted", text });
            }}
            style={freeTextStyle}
          />
        </label>
      )}
    </div>
  );
}
