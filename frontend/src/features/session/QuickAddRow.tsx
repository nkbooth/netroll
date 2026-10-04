// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { CSSProperties, FormEvent, ReactElement } from "react";
import { useStore } from "zustand";
import type { StoreApi } from "zustand/vanilla";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { BREAKPOINT_DESKTOP, layoutModeForWidth } from "../../ui/layout/responsive";
import { StatusIndicator } from "../../ui/components/StatusIndicator";
import { tokens } from "../../ui/tokens/tokens";
import { addCheckIn, lookupCheckInAutofill as defaultLookupRosterMemory } from "./sessionApi";
import type { RosterMemory } from "./sessionApi";
import { ViaPicker, sendableVia } from "./ViaPicker";
import { isPlausibleCallsign } from "../profile/callsignCheck";
import type { ViaWire } from "../nets/connectionPresentation";
import { selectDuplicateCheckIn, selectRoster } from "./sessionStore";
import type {
  DisplayRosterEntry,
  DuplicateCheckInState,
  SessionStore,
} from "./sessionStore";
import type { StayingStatus } from "./sessionWire";
import { reportShapeForMode } from "./signalReport";

/**
 * The pinned inline quick-add row — the hot-path heart of
 * the operator console. It replaces the basic `CheckInControl` and is the
 * FIRST live caller of the optimistic pending map:
 *
 * - Per commit it mints a `clientEventId` (`crypto.randomUUID()`), calls
 * `addPending` so a dimmed roster row renders INSTANTLY, then
 * `addCheckIn(sessionId, callsign, clientEventId)`. On success it does NOT
 * hand-set store state — the authoritative `checkin.added` echoing that id
 * reconciles the pending row through the shipped WS→reducer→`reconcilePending`
 * path.
 * - On a failed send it calls `removePending` to roll the optimistic row back
 * and surfaces the mapped `ProblemError`.
 * - It arms a bounded per-add timeout: if the echo never arrives (a delta
 * silently lost with no reconnect), the timer calls `removePending` so a
 * dimmed row cannot linger forever.
 *
 * Keyboard-first: a `<form>` so Enter = **Save & add new** (commit → clear
 * → refocus callsign, the rapid-fire loop); **Save & close** commits and stops
 * the loop (blur, no refocus — it does NOT close the session). The global `n`
 * hotkey focuses the callsign field from anywhere on the console EXCEPT while
 * the operator is typing in a field (callsigns contain `N`) or with a modifier
 * held. Callsign, Name, Location, Report, and Staying are wired end-to-end;
 * the Report input is mode-shaped and the Staying segmented
 * toggle captures the staying status at check-in. Live tab order: Callsign › Name ›
 * Location › Report › Staying › Save (see EXPERIENCE.md for the quick-add fields).
 *
 * Roster-memory prefill: on the callsign field's BLUR/tab-out
 * (not per keystroke — no typeahead dropdown, that is fenced out as YAGNI), a
 * best-effort `lookupRosterMemory` fills any EMPTY Name/Location field with the
 * name/location this net last logged the callsign with (seed-if-empty — never
 * clobbering operator-typed text). A miss, empty result, or failed lookup is a
 * SILENT no-op that never blocks the field or the commit. This is the LOCAL,
 * in-app half; QRZ autofill fills the SAME fields on the SAME
 * blur trigger, so the seam (`lookupRosterMemory`, injectable) is built for it.
 *
 * Which way in: the operator's session stamp — read from the
 * same store, never passed down — is what each check-in records, at no hot-path
 * cost. The `ViaPicker` below the Save buttons shows that stamp preselected and
 * carries the interleaved case: one tap overrides it for THIS station only, and
 * the selection returns to the stamp on commit. It sits AFTER the Save buttons
 * in DOM order deliberately — the documented tab order (Callsign › Name ›
 * Location › Grid › Report › Staying › Save) is pinned by a test that walks the
 * real focus order, and a control ahead of Save would move it.
 */

/**
 * The bounded window an optimistic entry waits for its authoritative echo before
 * the timeout rolls it back. The real-time SLO makes the round-trip near-
 * instant, so this is generous; it is the safety net for the one remaining gap
 * (a delta silently lost with no reconnect), not the normal reconcile path.
 */
const ECHO_TIMEOUT_MS = 8000;

/** Injectable timer (defaults to `window.setTimeout`) — fake-clock testable. */
type ScheduleTimeout = (fn: () => void, ms: number) => () => void;

const defaultScheduleTimeout: ScheduleTimeout = (fn, ms) => {
  const id = window.setTimeout(fn, ms);
  return () => window.clearTimeout(id);
};

/** Which station the duplicate warning is about, and which commit of ours (if any) put it on the roster. */
interface DuplicateSubject {
  readonly callsign: string;
  readonly ownClientEventId: string | null;
}

function rosterApartFromOwnRow(
  state: SessionStore,
  ownClientEventId: string | null,
): DisplayRosterEntry[] {
  const roster = selectRoster(state);
  if (ownClientEventId === null) {
    return roster;
  }
  // The row our commit produced is keyed by clientEventId while pending and by
  // checkInId once echoed — either one is the station matching itself.
  const ownCheckInId = state.echoed.get(ownClientEventId);
  return roster.filter(
    (entry) => entry.key !== ownClientEventId && entry.key !== ownCheckInId,
  );
}

/** True when a keydown target is a field the operator is editing (never hijack it). */
function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }
  const tag = target.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    target.isContentEditable
  );
}

/**
 * Below the desktop breakpoint (`BREAKPOINT_DESKTOP`, 640px — the SAME
 * breakpoint `ResponsiveList` reads), the quick-add collapses into a
 * one-handed thumb stack: callsign and both Save actions each go full-width
 * and stack vertically rather than sitting in a row (the phone ~340px case).
 * At/above 640px it stays a single inline row.
 */
const subscribeToWidth = (onChange: () => void): (() => void) => {
  window.addEventListener("resize", onChange);
  return () => window.removeEventListener("resize", onChange);
};

/** The row/stacked layout mode for the current viewport. */
function useQuickAddLayoutMode(): "row" | "stacked-card" {
  const width = useSyncExternalStore(
    subscribeToWidth,
    () => window.innerWidth,
    () => BREAKPOINT_DESKTOP,
  );
  return layoutModeForWidth(width).mode;
}

/** The outer form's chrome (design handoff `2b`'s pinned quick-add card) —
 * column layout so the eyebrow, the fields row, and the buttons/step row
 * each get their own line. */
const formStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-3)",
  padding: "var(--space-3) var(--space-4)",
  marginBottom: "var(--space-4)",
  background: "var(--surface-2)",
  borderLeft: "4px solid var(--accent)",
  borderRadius: "var(--rounded-md)",
};

const rowStyleBase: CSSProperties = {
  display: "flex",
  alignItems: "flex-end",
  gap: "var(--space-3)",
};

const rowStyleStacked: CSSProperties = {
  ...rowStyleBase,
  flexDirection: "column",
  alignItems: "stretch",
};

/** The "Quick add — next check-in" eyebrow (design handoff `2b`) — names the
 * `n` hotkey and, while the callsign field is focused, confirms it is ready. */
const eyebrowStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  flexWrap: "wrap",
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--accent-ink)",
};

const eyebrowHintStyle: CSSProperties = {
  color: "var(--text-muted)",
  fontWeight: 600,
  textTransform: "none",
  letterSpacing: "normal",
};

const eyebrowKeycapStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  minWidth: "1.4em",
  padding: "1px var(--space-1)",
  background: "var(--surface)",
  color: "var(--text-muted)",
  border: "1px solid var(--border)",
  borderBottomWidth: "2px",
  borderRadius: "var(--rounded-keycap)",
  fontFamily: tokens.typography.mono.fontFamily,
  fontSize: tokens.typography.keycap.fontSize,
  fontWeight: tokens.typography.keycap.fontWeight,
};

/** The field-order step indicator (design handoff `2b`) — a plain,
 * non-interactive reminder of the quick-add's tab order, not a per-field focus
 * tracker. */
const stepListStyle: CSSProperties = {
  display: "inline-flex",
  gap: "var(--space-1)",
  alignItems: "center",
  flexWrap: "wrap",
  marginLeft: "auto",
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  listStyle: "none",
  padding: 0,
  margin: 0,
};

const stepSeparatorStyle: CSSProperties = { opacity: 0.6 };

/** The quick-add's fixed field order, for the step indicator only.
 *
 * MEASURED against the rendered form on 2026-08-29, not copied from the design
 * handoff: Location and Grid are real tab stops, and this
 * list and `docs/live-sessions/log-check-ins.md` had each dropped one or both
 * of them while claiming to state the tab order. A test walks the real focus
 * order and holds this list to it. The trailing two steps each collapse a pair
 * of controls — the two Staying radios, and Save & add new / Save & close. */
const QUICK_ADD_STEPS = [
  "Callsign",
  "Name",
  "Location",
  "Grid",
  "Report",
  "Staying",
  "Save",
] as const;

const fieldStyleBase: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
  flex: "1 1 180px",
  // Without this, a flex item won't shrink below its CONTENT's intrinsic
  // width (the default `min-width: auto`) — an `<input>` with no explicit
  // width floors at the browser's default text-input size regardless of
  // `flex-basis`, so all four quick-add fields refused to shrink below
  // ~170-200px each and blew the whole row (and the page) out sideways on
  // any desktop window narrower than their fixed sum (real bug, not just a
  // narrow-viewport edge case).
  minWidth: 0,
};

/** The Callsign field's narrower basis (design handoff `2b`'s pinned quick-add:
 * `26px 1.7fr 1fr 92px 132px` — callsign gets the smallest of the text fields,
 * name/location grow, report stays narrow). */
const callsignFieldStyle: CSSProperties = { ...fieldStyleBase, flex: "0 1 140px" };

/** The Report field's narrower basis, matching the mock's ~92px report column. */
const reportFieldStyle: CSSProperties = { ...fieldStyleBase, flex: "0 1 110px" };

const fieldStyleStacked: CSSProperties = {
  ...fieldStyleBase,
  flex: "1 1 auto",
  width: "100%",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  color: "var(--text-muted)",
  textTransform: "uppercase",
  letterSpacing: "0.04em",
};

const baseInputStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-2)",
  background: "var(--surface)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  fontFamily: tokens.typography.mono.fontFamily,
  fontSize: "16px",
  fontWeight: 800,
  outline: "none",
};

const primaryButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const secondaryButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const buttonsStyleBase: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  flex: "0 0 auto",
};

const buttonsStyleStacked: CSSProperties = {
  ...buttonsStyleBase,
  flexDirection: "column",
  width: "100%",
};

/** Full-width in the stacked (phone) layout; otherwise unchanged. */
const stackedButtonWidth: CSSProperties = { width: "100%" };

const errorStyle: CSSProperties = {
  color: "var(--sync-text)",
  flexBasis: "100%",
  margin: 0,
};

/** The duplicate-warning line. Always mounted; empty on `none`. */
const duplicateWarningStyle: CSSProperties = {
  flexBasis: "100%",
  margin: 0,
  minHeight: "1lh",
};

/** The segmented staying toggle container (a radiogroup). */
const toggleGroupStyle: CSSProperties = {
  display: "inline-flex",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  overflow: "hidden",
};

/** One segment of the staying toggle; the selected one fills with the accent. */
function segmentStyle(selected: boolean): CSSProperties {
  return {
    padding: "var(--space-1) var(--space-3)",
    background: selected ? "var(--accent-deep)" : "transparent",
    color: selected ? "var(--on-accent)" : "var(--text)",
    border: "none",
    font: "inherit",
    fontSize: tokens.typography.labelCaps.fontSize,
    fontWeight: 700,
    cursor: "pointer",
  };
}

export interface QuickAddRowProps {
  readonly sessionId: string;
  /** The bound session store — the source of `addPending`/`removePending`. */
  readonly store: StoreApi<SessionStore>;
  /**
   * The net's operating mode token — `rfConnection(session.connections)?.mode`
   * — band and mode live on the session's connection set, not on the frozen
   * definition. Shapes the
   * report input's label/aria/placeholder. Absent → the
   * general free-form shape.
   */
  readonly mode?: string;
  /**
   * Whether the viewer may set staff-only fields — the signal report.
   * Defaults to `true`. A granted Relay holds `LogCheckIn` but NOT
   * `EditStaffFields`, so its quick-add HIDES the Report field (the server would
   * 403 a report from a relay anyway).
   * Callsign/Name/Location/Staying remain — a relay logs a callsign-only add.
   */
  readonly canSetReport?: boolean;
  /** Injectable bounded-timeout scheduler (fake-clock testable). */
  readonly scheduleTimeout?: ScheduleTimeout;
  /** Injectable client-event-id minter (defaults to `crypto.randomUUID`). */
  readonly mintClientEventId?: () => string;
  /**
   * Injectable prefill lookup — a best-effort, non-blocking
   * `(sessionId, callsign) => name/location | null`. The default now wires to
   * `lookupCheckInAutofill`, which merges the profile,
   * roster-memory, and callbook sources server-side;
   * the injectable prop and its seed-if-empty-on-blur contract are unchanged
   * (the seam built for exactly this layering).
   */
  readonly lookupRosterMemory?: (
    sessionId: string,
    callsign: string,
  ) => Promise<RosterMemory | null>;
}

/** The pinned, keyboard-first optimistic quick-add row. */
export function QuickAddRow({
  sessionId,
  store,
  mode,
  canSetReport = true,
  scheduleTimeout = defaultScheduleTimeout,
  mintClientEventId = () => crypto.randomUUID(),
  lookupRosterMemory = defaultLookupRosterMemory,
}: QuickAddRowProps): ReactElement {
  const [callsign, setCallsign] = useState("");
  const [name, setName] = useState("");
  const [location, setLocation] = useState("");
  const [grid, setGrid] = useState("");
  const [report, setReport] = useState("");
  // Most check-ins stay for the whole net — "staying for comments" is the
  // common case, so it's the default rather than "in and out". The operator
  // can still flip it per-station via the segmented control below, before
  // OR after saving (it's a regular field, not commit-then-fix-later).
  const [staying, setStaying] = useState<StayingStatus>("staying-for-comments");
  const [busy, setBusy] = useState(false);
  const [focused, setFocused] = useState(false);
  // `undefined` means "follow the stamp" — DISTINCT from `null`, which is the
  // operator saying THIS station records no way in at all. Collapsing the two
  // would make "not recorded" impossible to express while a stamp is in force.
  const [viaOverride, setViaOverride] = useState<ViaWire | null | undefined>(undefined);
  // WHO passed this station's traffic. Per-station and never
  // sticky — each relayed station is relayed by whoever happened to hear it, so
  // a session-level stamp would put a confidently-wrong third-party callsign on
  // every subsequent entry, which is strictly worse than a blank. The same
  // reasoning as `via`, applied to a field with no BLOCK case.
  const [relayedBy, setRelayedBy] = useState("");
  // Bumped on every successful commit to remount the picker, which is what
  // returns its free-text box to empty along with the selection (the picker
  // holds that locally, and reset-by-key is how a caller clears it).
  const [viaPickerGeneration, setViaPickerGeneration] = useState(0);
  const reportShape = reportShapeForMode(mode);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);
  // The duplicate warning's SUBJECT — set by the blur and
  // commit seams, cleared by the next keystroke. Only the subject is state. The
  // verdict is derived below from the roster as it stands, so a websocket delta
  // or a rolled-back send moves it instead of leaving a snapshot standing.
  const [duplicateSubject, setDuplicateSubject] = useState<DuplicateSubject | null>(null);
  const duplicateState = useStore(store, (state): DuplicateCheckInState =>
    duplicateSubject === null
      ? "none"
      : selectDuplicateCheckIn(
          rosterApartFromOwnRow(state, duplicateSubject.ownClientEventId),
          duplicateSubject.callsign,
        ),
  );
  const duplicateCallsign =
    duplicateSubject !== null && duplicateState !== "none" ? duplicateSubject.callsign : null;
  const inputRef = useRef<HTMLInputElement>(null);
  const viaStamp = useStore(store, (state) => state.viaStamp);
  const connections = useStore(store, (state) => state.session.connections);
  const viaInForce = viaOverride === undefined ? viaStamp : viaOverride;
  const layoutMode = useQuickAddLayoutMode();
  const stacked = layoutMode === "stacked-card";
  // Cancel fns for still-armed echo timers, keyed by the clientEventId each
  // would roll back on fire. Keyed (not a bare Set) so unmount can call
  // `removePending` itself, not just disarm the JS timer: `isLive` (and thus
  // this component) can flip away out from under an in-flight/un-echoed add
  // (e.g. another operator closes the net before OUR echo arrives) while the
  // surrounding store/page stay mounted — `seedFromSnapshot`'s wholesale
  // pending reset only fires on THIS client's own close/reconnect, so an
  // orphaned entry would otherwise render forever with no remaining path to
  // clear it once its timer is merely cancelled.
  const timersRef = useRef<Map<string, () => void>>(new Map());
  // Synchronous re-entrancy guard: one Enter commits one id exactly once, even
  // before the `busy` state re-render lands (the double-submit class).
  const submittingRef = useRef(false);

  useEffect(() => {
    const timers = timersRef.current;
    return () => {
      for (const [clientEventId, cancel] of timers) {
        cancel();
        store.getState().removePending(clientEventId);
      }
      timers.clear();
    };
  }, [store]);

  // The `n` hotkey. QuickAddRow only mounts on the live staff console, so
  // this document-level handler is inherently console-scoped. It must never
  // fire while the operator is typing in a field (callsigns contain `N`) or
  // with a modifier held.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key !== "n" && event.key !== "N") {
        return;
      }
      if (event.altKey || event.ctrlKey || event.metaKey) {
        return;
      }
      if (isEditableTarget(event.target)) {
        return;
      }
      event.preventDefault();
      inputRef.current?.focus();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const commit = async (refocus: boolean): Promise<void> => {
    if (submittingRef.current) {
      return;
    }
    const value = callsign.trim();
    if (value === "") {
      return;
    }
    // A blank-after-trim report is NO report (undefined), never an empty string
    // — matching the domain's optional-text idiom.
    const reportValue = report.trim();
    const signalReport = reportValue === "" ? undefined : reportValue;
    // Name/location (prefilled or operator-typed) — blank → undefined so a
    // callsign-only add still POSTs `{ callsign }` byte-for-byte.
    const nameTrimmed = name.trim();
    const nameValue = nameTrimmed === "" ? undefined : nameTrimmed;
    const locationTrimmed = location.trim();
    const locationValue = locationTrimmed === "" ? undefined : locationTrimmed;
    // Blank → undefined, so a grid-less add still POSTs no `grid`
    // key at all (the omit-optional rule name/location already follow). An
    // invalid grid is NOT checked here — the server's /errors/grid-invalid is
    // the single authority, and the modal/row already render its problem.
    const gridTrimmed = grid.trim();
    const gridValue = gridTrimmed === "" ? undefined : gridTrimmed;
    // The stamp, or this station's one-tap override. `sendableVia`
    // is the single answer to what may go on the wire — a blank free text and a
    // connection this session no longer lists both become NO KEY, because `via`
    // rides the same POST as the callsign and a refusal loses the station too.
    const viaValue = sendableVia(viaInForce, connections);
    // `relayedBy` rides the SAME POST as the callsign, so a server 400 on it
    // takes the whole check-in down with it. Anything the shipped
    // `isPlausibleCallsign` pre-check refuses — a pasted TAB, a bidi mark, a
    // typo — ships as NO KEY rather than as a value that costs the operator the
    // station. The server stays the only authority on what is valid.
    const relayedByTrimmed = relayedBy.trim();
    const relayedByValue = isPlausibleCallsign(relayedByTrimmed)
      ? relayedByTrimmed
      : undefined;
    submittingRef.current = true;
    setBusy(true);
    setProblem(null);

    // Mint/addPending/arm-timer live INSIDE the try (not before it), and the
    // ids/cancel-fn are declared OUTSIDE it (assigned as each step succeeds):
    // an unexpected throw partway through (e.g. `crypto.randomUUID`
    // unavailable in a non-secure context, or `addPending` itself throwing)
    // must still hit `finally` and release the busy/re-entrancy guard, and
    // `catch` must only roll back a pending entry that was actually added —
    // never call `removePending`/`cancelTimer` on an id that was never minted.
    let clientEventId: string | undefined;
    let cancelTimer: (() => void) | undefined;
    let forget: (() => void) | undefined;

    try {
      clientEventId = mintClientEventId();
      // The duplicate check runs on the commit path the
      // operator actually uses. Carrying the minted id is what keeps the row
      // this commit creates out of its own comparison.
      setDuplicateSubject({ callsign: value, ownClientEventId: clientEventId });
      // Optimistic: the dimmed roster row renders instantly, carrying the
      // report/staying the operator entered so
      // the pending row reflects them.
      // The way in rides along so the dimmed row shows what the
      // operator chose at once — a row that appeared with no way in would teach
      // them the stamp had not taken, on the surface meant to reassure them.
      store
        .getState()
        .addPending(
          clientEventId,
          value,
          signalReport ?? null,
          staying,
          "staff",
          viaValue ?? null,
          relayedByValue?.toUpperCase() ?? null,
        );

      // Arm the bounded echo timeout. Kept armed through a SUCCESSFUL send too:
      // if the authoritative echo is silently lost with no reconnect, this is
      // the only thing that clears the dimmed row. `removePending` is a
      // same-ref no-op once the echo reconciles, so a normal round-trip is
      // unaffected.
      const id = clientEventId;
      forget = (): void => {
        timersRef.current.delete(id);
      };
      cancelTimer = scheduleTimeout(() => {
        store.getState().removePending(id);
        forget?.();
      }, ECHO_TIMEOUT_MS);
      timersRef.current.set(id, cancelTimer);

      await addCheckIn(sessionId, {
        callsign: value,
        clientEventId,
        signalReport,
        staying,
        name: nameValue,
        location: locationValue,
        grid: gridValue,
        via: viaValue,
        relayedBy: relayedByValue,
      });
      // Success: let the authoritative echo reconcile the pending row — do NOT
      // hand-set store state. Clear the entry fields + reset staying to the
      // default, then (re)focus for the next station.
      setCallsign("");
      setName("");
      setLocation("");
      setGrid("");
      // The duplicate subject is deliberately NOT cleared here: on the Enter
      // fast path this commit is the first moment the check
      // has run, so clearing now would show the warning only for the length of
      // the round-trip. It names its station, and the next keystroke clears it.
      setReport("");
      setStaying("staying-for-comments");
      // Back to the block's connection with no second action: an override is
      // for ONE station, and the next one inherits the stamp again.
      setViaOverride(undefined);
      setViaPickerGeneration((generation) => generation + 1);
      // Cleared on every commit, unlike the way-in stamp: the next station was
      // relayed by someone else, or by nobody.
      setRelayedBy("");
      if (refocus) {
        inputRef.current?.focus();
      } else {
        inputRef.current?.blur();
      }
    } catch (error: unknown) {
      // Failure: roll the optimistic row back at once and disarm the timer —
      // but only if one was actually armed (a throw before minting/adding the
      // pending entry has nothing to roll back).
      if (clientEventId !== undefined) {
        store.getState().removePending(clientEventId);
        cancelTimer?.();
        forget?.();
      }
      setProblem(error instanceof ProblemError ? error.problem : undefined);
    } finally {
      submittingRef.current = false;
      setBusy(false);
    }
  };

  // On callsign blur/tab-out, best-effort prefill the EMPTY Name/
  // Location fields from the per-net roster memory (seed-if-empty). Non-blocking:
  // a miss/empty/failed lookup is a silent no-op that never blocks the field or
  // the commit. The functional setState guards seed-if-empty against a
  // stale closure — it reads the CURRENT field value, so operator-typed text is
  // never clobbered even if the lookup resolves after the operator has typed.
  //
  // The default lookup chains DB reads plus a possible QRZ/hamcall egress,
  // which widens the in-flight
  // window. Guard against a stale response: if the operator clears/retypes the
  // callsign and blurs again before this lookup resolves, a late result for the
  // OLD callsign must not seed fields now associated with a DIFFERENT one. The
  // input is controlled, so `inputRef.current.value` always reflects the live
  // DOM value at resolution time, unlike the closed-over `callsign` variable.
  const prefillFromMemory = async (): Promise<void> => {
    const value = callsign.trim();
    if (value === "") {
      return;
    }
    const remembered = await lookupRosterMemory(sessionId, value);
    if (remembered === null) {
      return;
    }
    if ((inputRef.current?.value ?? "").trim() !== value) {
      // The callsign changed while the lookup was in flight — this result is
      // for a station that is no longer the one being entered; drop it.
      return;
    }
    const rememberedName = remembered.name;
    if (rememberedName) {
      setName((current) => (current.trim() === "" ? rememberedName : current));
    }
    const rememberedLocation = remembered.location;
    if (rememberedLocation) {
      setLocation((current) => (current.trim() === "" ? rememberedLocation : current));
    }
  };

  const onSubmit = (event: FormEvent): void => {
    event.preventDefault();
    void commit(true);
  };

  return (
    <form
      onSubmit={onSubmit}
      style={formStyle}
      aria-label="Quick add check-in"
      data-layout-mode={layoutMode}
    >
      {/* The "Quick add" eyebrow (design handoff `2b`) — names the `n` hotkey
          that focuses the callsign field from anywhere on the console. */}
      <div role="group" aria-label="Quick add — next check-in" style={eyebrowStyle}>
        <span>Quick add — next check-in</span>
        <kbd style={eyebrowKeycapStyle}>n</kbd>
        {focused && <span style={eyebrowHintStyle}>focused &amp; ready</span>}
      </div>
      <div style={stacked ? rowStyleStacked : rowStyleBase}>
      <label style={stacked ? fieldStyleStacked : callsignFieldStyle}>
        <span style={labelStyle}>Callsign</span>
        <input
          ref={inputRef}
          type="text"
          autoCapitalize="characters"
          autoComplete="off"
          value={callsign}
          onChange={(event) => {
            setCallsign(event.target.value);
            setDuplicateSubject(null);
          }}
          onFocus={() => setFocused(true)}
          onBlur={() => {
            setFocused(false);
            // The blur seam. Deliberately NOT chained onto the
            // prefill lookup — no endpoint carries this answer, and a hot-path
            // warning behind a round-trip lands after the commit.
            const subject = callsign.trim();
            setDuplicateSubject(
              subject === "" ? null : { callsign: subject, ownClientEventId: null },
            );
            void prefillFromMemory();
          }}
          style={{
            ...baseInputStyle,
            width: stacked ? "100%" : undefined,
            boxShadow: focused ? "0 0 0 2px var(--accent)" : "none",
          }}
        />
      </label>
      <label style={stacked ? fieldStyleStacked : fieldStyleBase}>
        <span style={labelStyle}>Name</span>
        <input
          type="text"
          autoComplete="off"
          value={name}
          onChange={(event) => setName(event.target.value)}
          style={{ ...baseInputStyle, width: stacked ? "100%" : undefined }}
        />
      </label>
      <label style={stacked ? fieldStyleStacked : fieldStyleBase}>
        <span style={labelStyle}>Location</span>
        <input
          type="text"
          autoComplete="off"
          value={location}
          onChange={(event) => setLocation(event.target.value)}
          style={{ ...baseInputStyle, width: stacked ? "100%" : undefined }}
        />
      </label>
      <label style={stacked ? fieldStyleStacked : fieldStyleBase}>
        <span style={labelStyle}>Grid</span>
        <input
          type="text"
          autoCapitalize="characters"
          autoComplete="off"
          value={grid}
          onChange={(event) => setGrid(event.target.value)}
          style={{ ...baseInputStyle, width: stacked ? "100%" : undefined }}
        />
      </label>
      {/* The Report field is STAFF-ONLY (EditStaffFields). A granted Relay lacks
          it, so its console hides the field entirely — the
          server 403s a report from a relay regardless. */}
      {canSetReport && (
        <label style={stacked ? fieldStyleStacked : reportFieldStyle}>
          <span style={labelStyle}>{reportShape.label}</span>
          <input
            type="text"
            autoComplete="off"
            aria-label={reportShape.ariaHint}
            placeholder={reportShape.placeholder}
            value={report}
            onChange={(event) => setReport(event.target.value)}
            style={{ ...baseInputStyle, width: stacked ? "100%" : undefined }}
          />
        </label>
      )}
      <div
        role="radiogroup"
        aria-label="Staying status"
        style={{ ...(stacked ? fieldStyleStacked : fieldStyleBase), flex: "0 0 auto" }}
      >
        <span style={labelStyle}>Staying</span>
        <div style={toggleGroupStyle}>
          <button
            type="button"
            role="radio"
            aria-checked={staying === "staying-for-comments"}
            onClick={() => setStaying("staying-for-comments")}
            style={segmentStyle(staying === "staying-for-comments")}
          >
            Staying
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={staying === "in-and-out"}
            onClick={() => setStaying("in-and-out")}
            style={segmentStyle(staying === "in-and-out")}
          >
            In &amp; out
          </button>
        </div>
      </div>
      </div>
      <div style={stacked ? buttonsStyleStacked : buttonsStyleBase}>
        <button
          type="submit"
          disabled={busy}
          style={stacked ? { ...primaryButtonStyle, ...stackedButtonWidth } : primaryButtonStyle}
        >
          Save &amp; add new <span aria-hidden="true">↵</span>
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => void commit(false)}
          style={
            stacked ? { ...secondaryButtonStyle, ...stackedButtonWidth } : secondaryButtonStyle
          }
        >
          Save &amp; close
        </button>
        {/* The field-order step indicator (design handoff `2b`) — a plain
            reminder of the quick-add's tab order, not a per-field focus
            tracker (no per-field focus state is plumbed for this cosmetic
            hint). */}
        <ol aria-label="Field order" style={stepListStyle}>
          {QUICK_ADD_STEPS.map((step, index) => (
            <li key={step} style={{ display: "inline-flex", alignItems: "center", gap: "var(--space-1)" }}>
              {index > 0 && <span aria-hidden="true" style={stepSeparatorStyle}>›</span>}
              <span>{step}</span>
            </li>
          ))}
        </ol>
      </div>
      {/* The per-station override, AFTER the Save buttons in DOM
          order — the documented tab order is pinned by a test that walks the
          real focus order, and this must not move it. Remounted on each
          commit so its selection returns to the stamp. */}
      {/* WHO passed this station's traffic — AFTER the Save buttons
          in DOM order, exactly like the way-in picker above it, so the pinned
          tab order (Callsign → Name → Location → Grid → Report) does not move.
          A plain input, not a picker: a relaying station is not a connection of
          this net. */}
      <label style={{ display: "flex", flexDirection: "column", gap: "var(--space-1)" }}>
        <span style={labelStyle}>Relayed by</span>
        <input
          type="text"
          autoComplete="off"
          autoCapitalize="characters"
          placeholder="Callsign of the relaying station"
          value={relayedBy}
          onChange={(event) => setRelayedBy(event.target.value)}
          style={{ ...baseInputStyle, width: stacked ? "100%" : undefined }}
        />
      </label>
      <ViaPicker
        key={viaPickerGeneration}
        label="Way in"
        unsetLabel="Not recorded"
        connections={connections}
        value={viaInForce}
        onChange={setViaOverride}
      />
      {/* Mounted unconditionally — a polite live region that only
          appears when its text does is unreliably announced, and an assertive
          `role="alert"` here would interrupt a screen reader mid-hot-path. It
          advises; it never gates. */}
      <p
        role="status"
        aria-label="Duplicate check-in"
        data-duplicate-state={duplicateState}
        data-duplicate-callsign={duplicateCallsign ?? undefined}
        style={duplicateWarningStyle}
      >
        {duplicateState !== "none" && duplicateCallsign !== null && (
          <StatusIndicator
            tone="warn"
            icon={<span>&#9888;</span>}
            label={
              duplicateState === "unworked"
                ? `${duplicateCallsign} is already checked in — still waiting to be worked`
                : `${duplicateCallsign} was already checked in and worked earlier`
            }
          />
        )}
      </p>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </form>
  );
}
