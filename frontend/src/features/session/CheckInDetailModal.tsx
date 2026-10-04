// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { CSSProperties, FormEvent, KeyboardEvent, ReactElement } from "react";
import { useStore } from "zustand";
import type { StoreApi } from "zustand/vanilla";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import type { ViewerRole } from "./capabilities";
import { ModerationControls } from "./ModerationControls";
import {
  acquireLock as acquireLockApi,
  releaseLock as releaseLockApi,
  removeCheckIn as removeCheckInApi,
  updateCheckIn as updateCheckInApi,
} from "./sessionApi";
import { reportShapeForMode } from "./signalReport";
import { ViaPicker, sendableVia } from "./ViaPicker";
import type { ViaWire } from "../nets/connectionPresentation";
import type { DisplayRosterEntry, SessionStore } from "./sessionStore";
import type { Precedence, StayingStatus } from "./sessionWire";
import { tokens } from "../../ui/tokens/tokens";

/**
 * The check-in detail modal — the exception-path edit
 * surface for correcting a logged check-in. Opening it acquires the soft-lock
 * (broadcasting "X is editing…" to other consoles); a client heartbeat renews
 * the lease while open; Save commits the full editable field set with the
 * optimistic-concurrency `expectedVersion`; Cancel/Esc/unload release the lease
 * best-effort. Max 580px, 14px radius, surface-2 header, theme-split backdrop,
 * focus-trapped, Esc-dismissable, one level deep. All network
 * collaborators and the heartbeat scheduler are injectable for deterministic
 * unit tests.
 */

/** The heartbeat cadence — well inside the ~15s server TTL so an open modal
 * never lets its lease lapse. */
const HEARTBEAT_MS = 10_000;

type ScheduleHeartbeat = (fn: () => void, ms: number) => () => void;

const defaultScheduleHeartbeat: ScheduleHeartbeat = (fn, ms) => {
  const id = window.setInterval(fn, ms);
  return () => window.clearInterval(id);
};

/** Injectable collaborators (default to the real `sessionApi` + `setInterval`). */
export interface CheckInDetailModalDeps {
  acquireLock?: typeof acquireLockApi;
  releaseLock?: typeof releaseLockApi;
  updateCheckIn?: typeof updateCheckInApi;
  removeCheckIn?: typeof removeCheckInApi;
  scheduleHeartbeat?: ScheduleHeartbeat;
}

export interface CheckInDetailModalProps {
  readonly sessionId: string;
  /** The roster row being edited — the source of the seed values + CAS version. */
  readonly entry: DisplayRosterEntry;
  /** The bound session store — re-seeded from the folded summary on Save/Remove. */
  readonly store: StoreApi<SessionStore>;
  /** The net's operating mode, shaping the report input label/placeholder. */
  readonly mode?: string;
  /** The viewer's own resolved role — gates the moderation control
   * (`canViewerDo(viewerRole, "moderate")`); absent/`null` hides it. UX only. */
  readonly viewerRole?: ViewerRole | null;
  /** Closes the modal (the parent drops it from the tree). */
  readonly onClose: () => void;
  readonly deps?: CheckInDetailModalDeps;
}

const backdropStyle: CSSProperties = {
  position: "fixed",
  inset: 0,
  // Theme-split backdrop (DESIGN.md 230-231): dark default, light override via
  // the data-theme attribute the app stamps on <html>.
  background: "var(--modal-backdrop)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  padding: "var(--space-4)",
  zIndex: 1000,
};

const dialogStyle: CSSProperties = {
  width: "100%",
  maxWidth: "580px",
  background: "var(--surface)",
  color: "var(--text)",
  borderRadius: "var(--rounded-modal)",
  border: "1px solid var(--border)",
  overflow: "hidden",
};

const headerStyle: CSSProperties = {
  background: "var(--surface-2)",
  padding: "var(--space-3) var(--space-4)",
  fontWeight: 700,
};

/**
 * At/below this width the modal collapses to a single column;
 * above it, two columns. Distinct from the shared list breakpoints in
 * `ui/layout/responsive.ts` — this is the modal's OWN documented figure
 * (≤560px), not the roster's row/stacked breakpoint (640px).
 */
const MODAL_SINGLE_COLUMN_MAX_WIDTH = 560;

const subscribeToWidth = (onChange: () => void): (() => void) => {
  window.addEventListener("resize", onChange);
  return () => window.removeEventListener("resize", onChange);
};

/** The body grid's column count for the current viewport. */
function useModalColumns(): "one" | "two" {
  const width = useSyncExternalStore(
    subscribeToWidth,
    () => window.innerWidth,
    () => MODAL_SINGLE_COLUMN_MAX_WIDTH + 1,
  );
  return width <= MODAL_SINGLE_COLUMN_MAX_WIDTH ? "one" : "two";
}

const bodyStyleTwoColumn: CSSProperties = {
  display: "grid",
  gridTemplateColumns: "repeat(2, 1fr)",
  gap: "var(--space-3)",
  padding: "var(--space-4)",
};

const bodyStyleOneColumn: CSSProperties = {
  ...bodyStyleTwoColumn,
  gridTemplateColumns: "1fr",
};

const fieldStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  color: "var(--text-muted)",
  textTransform: "uppercase",
  letterSpacing: "0.04em",
};

const inputStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-2)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
};

const footerStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  alignItems: "center",
  padding: "var(--space-3) var(--space-4)",
  borderTop: "1px solid var(--border)",
};

const saveStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const ghostStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const removeStyle: CSSProperties = {
  ...ghostStyle,
  color: "var(--warn)",
  borderColor: "var(--warn)",
  marginRight: "auto",
};

const errorStyle: CSSProperties = { color: "var(--sync-text)", margin: 0, gridColumn: "1 / -1" };

const toggleGroupStyle: CSSProperties = {
  display: "inline-flex",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  overflow: "hidden",
};

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

const FOCUSABLE =
  'button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** The check-in detail modal component. */
export function CheckInDetailModal({
  sessionId,
  entry,
  store,
  mode,
  viewerRole = null,
  onClose,
  deps = {},
}: CheckInDetailModalProps): ReactElement {
  const acquireLock = deps.acquireLock ?? acquireLockApi;
  const releaseLock = deps.releaseLock ?? releaseLockApi;
  const updateCheckIn = deps.updateCheckIn ?? updateCheckInApi;
  const removeCheckIn = deps.removeCheckIn ?? removeCheckInApi;
  const scheduleHeartbeat = deps.scheduleHeartbeat ?? defaultScheduleHeartbeat;

  const checkInId = entry.key;
  const [callsign, setCallsign] = useState(entry.callsign);
  const [name, setName] = useState(entry.name ?? "");
  const [location, setLocation] = useState(entry.location ?? "");
  const [grid, setGrid] = useState(entry.grid ?? "");
  const [report, setReport] = useState(entry.signalReport ?? "");
  const [staying, setStaying] = useState<StayingStatus>(entry.staying);
  const [precedence, setPrecedence] = useState<Precedence>(entry.precedence);
  const [traffic, setTraffic] = useState(entry.traffic === null ? "" : String(entry.traffic));
  const [notes, setNotes] = useState(entry.notes ?? "");
  // The per-station note SPLIT. `notes` above is now the STAFF note
  // (the persisted key is unchanged, which is what keeps every note written
  // before this story private); this is the observer-facing one.
  const [publicNote, setPublicNote] = useState(entry.publicNote ?? "");
  // `via` is three-state on the wire and the three states mean
  // different things, so the EDITOR needs a dirty flag the other fields do not.
  // An untouched `via` must be OMITTED — sending `null` on every unrelated save
  // would clear a way in nobody was editing (the field-wipe defect, one field
  // over, guarded by never sending the key at all). Only a touched one is sent, as a value or as an explicit `null`.
  const [via, setVia] = useState<ViaWire | null>(entry.via);
  const [viaDirty, setViaDirty] = useState(false);
  // `relayedBy` is three-state on the wire for the same reason
  // `via` is — omitted keeps, `null` clears, a value replaces. Prefilled from
  // the entry so the field READS as what is stored; `relayedByDirty` is what
  // decides whether the key is sent at all, because an untouched field must not
  // rewrite a value this operator never looked at.
  const [relayedBy, setRelayedBy] = useState(entry.relayedBy ?? "");
  const [relayedByDirty, setRelayedByDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);
  // Set when a `lock-held` 409 is seen — on the initial acquire OR a later
  // heartbeat renewal (another operator won the lease after ours lapsed).
  // Disables Save/Remove so the operator isn't left filling out a form that
  // can only ever be refused.
  const [lockHeld, setLockHeld] = useState(false);
  const reportShape = reportShapeForMode(mode);
  // Resolved at RENDER against the session's own connections, never from the
  // row's frozen label — a mid-net QSY re-labels what the operator is editing.
  const connections = useStore(store, (state) => state.session.connections);
  const columns = useModalColumns();
  const dialogRef = useRef<HTMLDivElement>(null);
  const firstFieldRef = useRef<HTMLInputElement>(null);
  // The lease is released AT MOST ONCE — explicitly on close/Esc/Cancel, on
  // page unload, and on unmount as the
  // backstop. The ref guards against a double DELETE across those paths.
  const releasedRef = useRef(false);
  // Guards the post-`await` state updates in onSave/onRemove: if the roster
  // row vanished out from under the modal (another client's `checkin.removed`
  // folded in, unmounting this modal via LiveSessionPage's `editingEntry`
  // lookup) while a Save/Remove was in flight, the response must not touch
  // state on an unmounted component.
  const mountedRef = useRef(true);
  useEffect(
    () => (): void => {
      mountedRef.current = false;
    },
    [],
  );

  const release = useCallback(() => {
    if (releasedRef.current) {
      return;
    }
    releasedRef.current = true;
    void releaseLock(sessionId, checkInId).catch(() => {});
  }, [releaseLock, sessionId, checkInId]);

  // Acquire the lease on open, then renew it on a heartbeat while the modal is
  // open. Release on page unload AND on unmount (the backstop for a parent that
  // drops the modal without calling close); the ~15s TTL is the honest fallback
  // if none of those fire.
  useEffect(() => {
    let active = true;
    const onAcquireFailure = (error: unknown): void => {
      if (!active) {
        return;
      }
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      if (error instanceof ProblemError && error.problem.type === "/errors/lock-held") {
        setLockHeld(true);
      }
    };
    void acquireLock(sessionId, checkInId).catch(onAcquireFailure);
    const cancel = scheduleHeartbeat(() => {
      // A renewal can fail too — e.g. a throttled tab misses enough
      // heartbeats that another operator's acquire wins the lapsed lease.
      // Surface it the same way as the initial failure rather than swallowing
      // it silently, so the operator isn't left editing a form that can only
      // be refused on Save.
      void acquireLock(sessionId, checkInId).catch(onAcquireFailure);
    }, HEARTBEAT_MS);
    window.addEventListener("pagehide", release);
    return () => {
      active = false;
      cancel();
      window.removeEventListener("pagehide", release);
      release();
    };
  }, [acquireLock, scheduleHeartbeat, sessionId, checkInId, release]);

  // Autofocus the first field on open.
  useEffect(() => {
    firstFieldRef.current?.focus();
  }, []);

  const close = useCallback(() => {
    // Explicit best-effort release on close/Esc/Cancel; the unmount
    // cleanup's `release()` is then a guarded no-op.
    release();
    onClose();
  }, [release, onClose]);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (event.key === "Escape") {
      event.preventDefault();
      close();
      return;
    }
    if (event.key !== "Tab") {
      return;
    }
    // Focus trap: wrap Tab / Shift+Tab within the dialog.
    const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE);
    if (focusable === undefined || focusable.length === 0) {
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const activeEl = document.activeElement;
    if (event.shiftKey && activeEl === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && activeEl === last) {
      event.preventDefault();
      first.focus();
    }
  };

  // An emptied box is an explicit CLEAR, so it must reach the wire as `null`
  // rather than dropping out of JSON.stringify. The server reads an
  // ABSENT key as "keep the stored value" — that is what stops the public page's
  // three-field staying toggle from wiping what staff logged — so an omitted key
  // would silently turn this clear into a no-op.
  const trimmedOrNull = (value: string): string | null => {
    const trimmed = value.trim();
    return trimmed === "" ? null : trimmed;
  };

  const onSave = async (event: FormEvent): Promise<void> => {
    event.preventDefault();
    if (busy || lockHeld) {
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      const summary = await updateCheckIn(sessionId, checkInId, {
        callsign: callsign.trim(),
        // An emptied name/location clears server-side; send "" (not undefined)
        // so PUT-replace drops the prior value rather than skipping the key.
        name: name.trim(),
        location: location.trim(),
        // Same PUT-replace clear-by-"" contract as name/location: the server's
        // `parse_edit_grid` treats a blank-after-trim value as "clear", which is
        // why it cannot simply hand "" to `parse_grid` (that returns Err(Empty)).
        grid: grid.trim(),
        signalReport: trimmedOrNull(report),
        staying,
        precedence,
        // Blank → null (clear the traffic); a numeric string → its integer
        // value. The server validates the bound (0..=999) and rejects 400.
        traffic: traffic.trim() === "" ? null : Number(traffic),
        // An emptied note clears server-side; send trimmed "" (PUT-replace), like
        // name/location. Both notes follow the same contract.
        notes: notes.trim(),
        publicNote: publicNote.trim(),
        // Omitted when untouched (keep), `null` when cleared, a value when
        // changed. A touched `via` the session cannot resolve — or a blank
        // free text — clears rather than posting something the server refuses.
        ...(viaDirty ? { via: sendableVia(via, connections) ?? null } : {}),
        // The same tri-state. A touched-then-emptied field sends an
        // explicit `null` — this is the exception-path surface where a mis-entry
        // gets REMOVED, so a clear must reach the server as a clear.
        ...(relayedByDirty ? { relayedBy: trimmedOrNull(relayedBy) } : {}),
        expectedVersion: entry.version,
      });
      // The store re-seed always applies (it is the authoritative summary,
      // and `seedFromSnapshot` mutates the external Zustand store, not this
      // component's own state). But if the row was removed by another client
      // while this Save was in flight, LiveSessionPage has already unmounted
      // the modal (its `editingEntry` lookup dropped out) — only `close()`
      // when we are still mounted, so a stale unmount doesn't double-fire
      // `onClose`/`release`.
      store.getState().seedFromSnapshot(summary);
      if (mountedRef.current) {
        close();
      }
    } catch (error: unknown) {
      if (mountedRef.current) {
        setProblem(error instanceof ProblemError ? error.problem : undefined);
      }
    } finally {
      if (mountedRef.current) {
        setBusy(false);
      }
    }
  };

  const onRemove = async (): Promise<void> => {
    if (busy || lockHeld) {
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      const summary = await removeCheckIn(sessionId, checkInId, entry.version);
      store.getState().seedFromSnapshot(summary);
      if (mountedRef.current) {
        close();
      }
    } catch (error: unknown) {
      if (mountedRef.current) {
        setProblem(error instanceof ProblemError ? error.problem : undefined);
      }
    } finally {
      if (mountedRef.current) {
        setBusy(false);
      }
    }
  };

  return (
    <div
      style={backdropStyle}
      data-testid="checkin-modal-backdrop"
      onMouseDown={(event) => {
        // A click on the backdrop (outside the dialog) cancels — one level deep.
        if (event.target === event.currentTarget) {
          close();
        }
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="checkin-modal-title"
        style={dialogStyle}
        onKeyDown={onKeyDown}
      >
        <div id="checkin-modal-title" style={headerStyle}>
          Edit check-in
        </div>
        <form onSubmit={(event) => void onSave(event)}>
          <div
            style={columns === "one" ? bodyStyleOneColumn : bodyStyleTwoColumn}
            data-columns={columns}
            data-testid="checkin-modal-body"
          >
            <label style={fieldStyle}>
              <span style={labelStyle}>Callsign</span>
              <input
                ref={firstFieldRef}
                type="text"
                autoComplete="off"
                autoCapitalize="characters"
                value={callsign}
                onChange={(event) => setCallsign(event.target.value)}
                style={inputStyle}
              />
            </label>
            <label style={fieldStyle}>
              <span style={labelStyle}>Name</span>
              <input
                type="text"
                autoComplete="off"
                value={name}
                onChange={(event) => setName(event.target.value)}
                style={inputStyle}
              />
            </label>
            <label style={fieldStyle}>
              <span style={labelStyle}>Location</span>
              <input
                type="text"
                autoComplete="off"
                value={location}
                onChange={(event) => setLocation(event.target.value)}
                style={inputStyle}
              />
            </label>
            <label style={fieldStyle}>
              <span style={labelStyle}>Grid</span>
              <input
                type="text"
                autoComplete="off"
                autoCapitalize="characters"
                value={grid}
                onChange={(event) => setGrid(event.target.value)}
                style={inputStyle}
              />
            </label>
            {/* WHICH way in this station came in on, now
                EDITABLE through the ordinary PUT — no new
                correction event and no change to the event catalogue
                (architecture ruling #4). The same control the quick-add
                captures with, so the label function and the free-text bound
                cannot drift between capture and correction. Mounted always,
                including for an entry nobody recorded a way in for: this is the
                exception-path surface where that omission gets fixed. */}
            <div style={fieldStyle} data-via>
              <ViaPicker
                label="Came in on"
                unsetLabel="Not recorded"
                connections={connections}
                value={via}
                onChange={(next) => {
                  setVia(next);
                  setViaDirty(true);
                }}
              />
            </div>
            {/* WHO passed this station's traffic. A plain
                single-line callsign field, deliberately NOT a `ViaPicker`:
                a relaying station is not a connection of this net, and reaching
                for that control is the first sign the field has been modelled as
                one. Mounted always, including for an entry nobody relayed —
                this is where that omission gets fixed. */}
            <label style={fieldStyle}>
              <span style={labelStyle}>Relayed by</span>
              <input
                type="text"
                autoComplete="off"
                autoCapitalize="characters"
                placeholder="Callsign of the relaying station"
                value={relayedBy}
                onChange={(event) => {
                  setRelayedBy(event.target.value);
                  setRelayedByDirty(true);
                }}
                style={inputStyle}
              />
            </label>
            <label style={fieldStyle}>
              <span style={labelStyle}>{reportShape.label}</span>
              <input
                type="text"
                autoComplete="off"
                aria-label={reportShape.ariaHint}
                placeholder={reportShape.placeholder}
                value={report}
                onChange={(event) => setReport(event.target.value)}
                style={inputStyle}
              />
            </label>
            <div role="radiogroup" aria-label="Staying status" style={fieldStyle}>
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
            <div role="radiogroup" aria-label="Precedence" style={fieldStyle}>
              <span style={labelStyle}>Precedence</span>
              <div style={toggleGroupStyle}>
                {(["routine", "priority", "emergency"] as const).map((value) => (
                  <button
                    key={value}
                    type="button"
                    role="radio"
                    aria-checked={precedence === value}
                    onClick={() => setPrecedence(value)}
                    style={segmentStyle(precedence === value)}
                  >
                    {value === "routine"
                      ? "Routine"
                      : value === "priority"
                        ? "Priority"
                        : "Emergency"}
                  </button>
                ))}
              </div>
            </div>
            <label style={fieldStyle}>
              <span style={labelStyle}>Traffic</span>
              <input
                type="number"
                inputMode="numeric"
                min={0}
                max={999}
                autoComplete="off"
                aria-label="Traffic count"
                value={traffic}
                onChange={(event) => setTraffic(event.target.value)}
                style={inputStyle}
              />
            </label>
            {/* The two per-station notes —
                running round commentary, each spanning the full grid width. Both
                ride the same edit path, the same soft lock and the same
                validation, and neither derives a correction.

                The STAFF field is the one that already existed, RELABELLED: an
                operator who has been writing notes for months must be able to
                see, without reading a changelog, that the box they know is the
                private one and that a new, public box sits beside it. Everything
                already written stayed here. */}
            <label style={{ ...fieldStyle, gridColumn: "1 / -1" }}>
              <span style={labelStyle}>Staff note (operators only)</span>
              <textarea
                autoComplete="off"
                value={notes}
                onChange={(event) => setNotes(event.target.value)}
                style={{ ...inputStyle, minHeight: "3em", resize: "vertical" }}
              />
            </label>
            <label style={{ ...fieldStyle, gridColumn: "1 / -1" }}>
              <span style={labelStyle}>Public note (everyone watching)</span>
              <textarea
                autoComplete="off"
                value={publicNote}
                onChange={(event) => setPublicNote(event.target.value)}
                style={{ ...inputStyle, minHeight: "3em", resize: "vertical" }}
              />
            </label>
            {problem !== null && (
              <p role="alert" style={errorStyle}>
                {messageForProblem(problem)}
              </p>
            )}
          </div>
          <div style={footerStyle}>
            <button
              type="button"
              style={removeStyle}
              disabled={busy || lockHeld}
              onClick={() => void onRemove()}
            >
              Remove
            </button>
            <button type="button" style={ghostStyle} disabled={busy} onClick={close}>
              Cancel
            </button>
            <button type="submit" style={saveStyle} disabled={busy || lockHeld}>
              Save
            </button>
          </div>
          {/* The NCS disciplinary remove/block — a DISTINCT,
              higher-bar act from the correction Remove above (renders nothing
              unless the viewer holds `moderate`). On success the store re-seeds
              from the folded summary and the modal closes, exactly like onRemove. */}
          <ModerationControls
            sessionId={sessionId}
            checkInId={checkInId}
            version={entry.version}
            source={entry.source}
            viewerRole={viewerRole}
            onModerated={(summary) => {
              store.getState().seedFromSnapshot(summary);
              if (mountedRef.current) {
                close();
              }
            }}
          />
        </form>
      </div>
    </div>
  );
}
