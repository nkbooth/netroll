// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { absoluteLocalTime, humanizeTime } from "../../ui/util/humanizeTime";
import { PrecedenceChip } from "./PrecedenceChip";
import { SourceBadge } from "./SourceBadge";
import { StayingIndicator } from "./StayingIndicator";
import { WorkingCursor } from "./WorkingCursor";
import { YourTurnIndicator } from "./YourTurnIndicator";
import type { Correction } from "./sessionReducer";
import type { DisplayRosterEntry } from "./sessionStore";
import { tokens } from "../../ui/tokens/tokens";

/**
 * One roster row. The callsign in the monospace radio-data face, stable
 * `seq` order. An optimistic (not-yet-echoed) entry is dimmed and flagged via
 * `data-optimistic`. On the operator console (`showSource`) it also carries the
 * check-in source badge, the mode-shaped signal report, the staying
 * indicator, the amber correction annotations + an Edit affordance,
 * and — when ANOTHER operator holds the soft-lock — a read-only amber-wash lock
 * flag with the edit control disabled.
 *
 * `showSource` is the STAFF-console flag, NOT "the public surface": the
 * observer surfaces pass `showStaying` and `showPrecedence`, and the public
 * note renders unconditionally, while the signal report, the soft-lock flag,
 * the correction annotations, the working-cursor control and Edit stay
 * operator-only.
 *
 * Layout (design handoff `2a`/`2b`): flex cells at the SAME proportions as the
 * mock's fixed `grid-template-columns` (participant `26px 1.9fr 92px 84px 74px`,
 * operator adds a 96px Precedence cell) — reproduced as `flex-basis` on plain
 * flex children rather than switching to CSS grid, so the existing flex-wrap
 * responsive behavior (`ResponsiveList`/`responsive.ts`) stays intact. Fields the
 * mock has no column for (staying, traffic, lock flag, corrections, the working/
 * edit affordances) nest inside the growing "info" cell or trail the fixed
 * columns, wrapping onto their own line when space runs out.
 */

const rowStyle: CSSProperties = {
  display: "flex",
  alignItems: "baseline",
  gap: "var(--space-3)",
  padding: "var(--space-2) 0",
  borderBottom: "1px solid var(--border)",
  listStyle: "none",
};

/**
 * The roster's cell widths, as `flex` shorthands at the SAME proportions as the
 * mock's fixed `grid-template-columns`. Exported because `RosterColumnHeads`
 * lays its labels out on these exact bases — a head strip built on a second,
 * hand-copied set of widths is a head strip that drifts out of alignment.
 */
export const ROSTER_CELL_BASIS = {
  position: "0 0 26px",
  info: "1 1 160px",
  badge: "0 1 92px",
  report: "0 1 84px",
  precedence: "0 1 96px",
  heard: "0 0 74px",
} as const;

/** The list-position ordinal cell — a muted mono index, fixed
 * to the mock's 26px `#` column. */
const positionStyle: CSSProperties = {
  flex: ROSTER_CELL_BASIS.position,
  color: "var(--text-muted)",
  textAlign: "center",
};

/** The growing "Station · operator" info cell — callsign/name plus the
 * working/next-up/lock/correction sub-lines, matching the mock's col2. */
const infoCellStyle: CSSProperties = {
  flex: ROSTER_CELL_BASIS.info,
  minWidth: 0,
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
};

/** The Source badge cell — fixed to the mock's 92px column. */
const badgeCellStyle: CSSProperties = { flex: ROSTER_CELL_BASIS.badge };

/** The report cell — mono radio-data face, fixed to the mock's 84px column
  */
const reportCellStyle: CSSProperties = { flex: ROSTER_CELL_BASIS.report };

const reportStyle: CSSProperties = {
  fontWeight: 800,
  color: "var(--text-muted)",
};

/** The Precedence cell — fixed to the mock's 96px column; the traffic count
 * rides alongside the chip since the mock reserves no separate
 * column for it.
 *
 * Gated `showSource || showPrecedence`, so it renders on the public roster
 * too. The 96px basis comes from the OPERATOR mock, the widest surface that
 * carries it. */
const precedenceCellStyle: CSSProperties = {
  flex: ROSTER_CELL_BASIS.precedence,
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
};

/** The traffic-count face — mono, under the precedence chip. */
const trafficStyle: CSSProperties = {
  fontWeight: 800,
  color: "var(--text-muted)",
};

/** The Heard cell — a muted, right-aligned relative time since check-in,
 * fixed to the mock's 74px column (both the 5-col participant and 6-col
 * operator layouts end on this column). */
const heardCellStyle: CSSProperties = {
  flex: ROSTER_CELL_BASIS.heard,
  textAlign: "right",
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
};

/** WHICH way in the station came in on — a sub-line in the same
 * meta face as the correction annotations it sits beside, since it answers the
 * same kind of question about the row. */
const viaStyle: CSSProperties = {
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
};

/** WHICH STATION passed this station's traffic — its own sub-line
 * beside the way in, in the same meta face, because the two answer different
 * questions about the row and collapsing them would be the very ambiguity the
 * separate field exists to remove. */
const relayedByStyle: CSSProperties = {
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
};

/** The per-station PUBLIC note — a wrapping prose sub-line under
 * the callsign, in the meta face. Full-strength muted text, not a dimmed one:
 * it is written to be read by someone who is not an operator. */
const publicNoteStyle: CSSProperties = {
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
  overflowWrap: "anywhere",
  whiteSpace: "pre-wrap",
};

/** The amber "{callsign} is editing…" lock flag. */
const lockFlagStyle: CSSProperties = {
  color: "var(--warn)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
};

/** The amber correction annotation (`correction-note: warn`). */
const correctionStyle: CSSProperties = {
  color: "var(--warn)",
  fontSize: tokens.typography.meta.fontSize,
};

/** The green worked-tick — `--staying` green, color+icon+label. */
const workedTickStyle: CSSProperties = {
  color: "var(--staying)",
  display: "inline-flex",
  alignItems: "center",
};

/** A check mark for a completed (worked) station. */
const workedTickIcon: ReactNode = (
  <svg
    width="12"
    height="12"
    viewBox="0 0 10 10"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    aria-hidden="true"
  >
    <path d="M2 5.5 L4 7.5 L8 2.5" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

const editButtonStyle: CSSProperties = {
  marginLeft: "auto",
  padding: "var(--space-1) var(--space-3)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

/** Human labels for the correction field tokens. */
const FIELD_LABELS: Record<Correction["field"], string> = {
  callsign: "callsign",
  name: "name",
  location: "location",
  grid: "grid",
  "signal-report": "report",
  staying: "staying",
  precedence: "precedence",
  traffic: "traffic",
  via: "the way in",
  relayedBy: "the relaying station",
};

/** The LATEST correction per field, newest-wins (renders one note per field). */
function latestCorrectionsPerField(
  corrections: readonly Correction[],
): Correction[] {
  const byField = new Map<Correction["field"], Correction>();
  for (const correction of corrections) {
    byField.set(correction.field, correction);
  }
  return [...byField.values()];
}

export interface RosterEntryProps {
  readonly entry: DisplayRosterEntry;
  /**
   * Render the operator-console affordances — source badge, report, staying,
   * corrections, the Edit control, and the working/worked treatments.
   * Defaults to `false` so the public view shows none of them.
   */
  readonly showSource?: boolean;
  /** Opens the detail modal for this entry (operator console only). */
  readonly onEdit?: () => void;
  /** Toggles the working cursor on this entry. Passing it renders
   * the inline `w` set-working control on the operator surface. */
  readonly onSetWorking?: (key: string) => void;
  /** Marks this entry the selected row — the `w` hotkey target.
   * Wired to click/focus; passing it makes the row focusable/selectable. */
  readonly onSelect?: (key: string) => void;
  /** Whether this row is the currently selected one (for the `w` hotkey). */
  readonly selected?: boolean;
  /**
   * Render the source badge WITHOUT the rest of the staff affordances — the
   * account-less public roster advertises provenance (Self/Staff).
   * `showSource` implies this. The public view passes
   * `showStaying`/`showPrecedence` beside it; the report and the edit controls
   * stay operator-only.
   */
  readonly showSourceBadge?: boolean;
  /**
   * Render the worked/working coral treatment WITHOUT the rest of the staff
   * affordances — the public view shows the worked-station
   * highlight (completing) but no interactive set-working control (which is
   * gated on `onSetWorking`, never passed publicly). `showSource` implies this.
   */
  readonly showWorked?: boolean;
  /** The 1-based list position (the stable roster ordinal), rendered for every
   * viewer when provided. */
  readonly position?: number;
  /** Render the YourTurnIndicator on this row — set by the
   * pure `selectYourTurnCheckInId` selector when this row is the viewer's own
   * next-up entry. Adds the cyan left-bar + wash + "You" chip + note. */
  readonly yourTurn?: boolean;
  /** The reference instant the Heard column's relative time is computed
   * against. Defaults to the render instant — inject an explicit `Date` for
   * a deterministic test. */
  readonly now?: Date;
  /**
   * Render the Heard cell as an absolute local time (`absoluteLocalTime`)
   * instead of the relative "Xm/h/d ago" phrase (the post-net summary's
   * "Checked" column — a closed session's history reads better as a fixed
   * clock time than a relative-to-viewing-instant phrase).
   */
  readonly heardAbsolute?: boolean;
  /**
   * Render the Report cell WITHOUT the rest of the staff affordances —
   * `showSource` implies this too. Lets a non-interactive summary surface
   * (the post-net roster table) show Report/Precedence alongside the source
   * badge without the staying indicator, lock flag, corrections, working
   * cursor, or Edit control, none of which apply once a session is closed.
   */
  readonly showReport?: boolean;
  /** Render the Precedence (+ traffic) cell WITHOUT the rest of the staff
   * affordances — `showSource` implies this too. See `showReport`. */
  readonly showPrecedence?: boolean;
  /**
   * Render the staying indicator WITHOUT the rest of the staff affordances
   * — `showSource` implies this too, exactly as it does for
   * `showReport`/`showPrecedence`.
   *
   * A DISTINCT prop rather than a widening of `showSource`: that flag also
   * carries the soft-lock flag, the correction annotations, the working-cursor
   * control and the Edit affordance, none of which belong on an observer's
   * roster. Passing it separately is what keeps the observer surface read-only.
   */
  readonly showStaying?: boolean;
}

/** A single roster station row. */
export function RosterEntry({
  entry,
  showSource = false,
  onEdit,
  onSetWorking,
  onSelect,
  selected = false,
  showSourceBadge = false,
  showWorked = false,
  position,
  yourTurn = false,
  now = new Date(),
  heardAbsolute = false,
  showReport = false,
  showPrecedence = false,
  showStaying = false,
}: RosterEntryProps): ReactElement {
  const lockedByOther = entry.lockedBy !== null;
  const corrections = showSource ? latestCorrectionsPerField(entry.corrections) : [];
  // The source badge and the worked/working treatment render on the public view
  // too: `showSource` (staff) implies both; the public view
  // opts into them individually via `showSourceBadge` / `showWorked`.
  //
  // An account-less viewer gets no WRITE affordance; visibility is a separate
  // question from that, and staying, precedence, the traffic count and the
  // public note are all visible to an observer.
  const badge = showSource || showSourceBadge;
  const workedTreatment = showSource || showWorked;
  // Working takes visual precedence over worked.
  const isWorking = workedTreatment && entry.working;
  const isWorked = workedTreatment && entry.worked && !entry.working;
  // Row background precedence: the coral working wash, else the cyan your-turn
  // wash, else another operator's amber lock wash, else none. Opacity: optimistic
  // dim, else worked dim, else 1. (Working and your-turn are mutually exclusive —
  // your-turn is the entry AFTER the cursor — but working wins defensively.)
  const background = isWorking
    ? "color-mix(in srgb, var(--cursor) 14%, transparent)"
    : yourTurn
      ? "color-mix(in srgb, var(--accent) 9%, transparent)"
      : lockedByOther
        ? "color-mix(in srgb, var(--warn) 12%, transparent)"
        : undefined;
  const opacity = entry.optimistic ? 0.6 : isWorked ? 0.55 : 1;
  // A still-pending (optimistic) row has no real checkInId yet — excluded
  // from selection the same way the inline set-working control already
  // excludes it (review finding: selecting a pending row and pressing `w`
  // would send a bogus/temporary key to the server).
  const selectable = showSource && !entry.optimistic && onSelect !== undefined;
  return (
    <li
      data-optimistic={String(entry.optimistic)}
      data-locked={String(lockedByOther)}
      data-working={String(isWorking)}
      data-worked={String(isWorked)}
      data-your-turn={String(yourTurn)}
      data-selected={selectable ? String(selected) : undefined}
      // `aria-current` (valid on any element) marks the `w`-hotkey target row;
      // `aria-selected` is NOT allowed on a `listitem` role and would fail the
      // axe floor, so it is deliberately not used here.
      aria-current={selectable && selected ? "true" : undefined}
      tabIndex={selectable ? 0 : undefined}
      onClick={selectable ? () => onSelect(entry.key) : undefined}
      onFocus={selectable ? () => onSelect(entry.key) : undefined}
      style={{
        ...rowStyle,
        opacity,
        background,
        // The 4px left-bar: coral marks the working station, cyan marks
        // the viewer's own next-up row (DESIGN.md:197-203).
        borderLeft: isWorking
          ? "var(--space-status-bar) solid var(--cursor)"
          : yourTurn
            ? "var(--space-status-bar) solid var(--accent)"
            : undefined,
        flexWrap: "wrap",
      }}
    >
      {position !== undefined && (
        <span className="mono" data-position style={positionStyle}>
          {position}
        </span>
      )}
      {/* The growing "Station · operator" info cell (mock col2): callsign on
          its own line, then the working/next-up/lock/correction sub-lines —
          everything the mock's fixed columns have no slot for. */}
      <div style={infoCellStyle}>
        <span style={{ display: "inline-flex", alignItems: "center", gap: "var(--space-2)" }}>
          <span className="mono" data-callsign>
            {entry.callsign}
          </span>
          {/* `role="img"` on the tick below is load-bearing, not decoration:
              `aria-label` is PROHIBITED on a bare span (role=generic) and axe
              fails it. The worked block collapses behind a
              disclosure and runs axe over the EXPANDED group, which is what
              first put a WORKED row under the accessibility gate — the baseline
              RosterEntry axe case renders an unworked entry only. */}
          {isWorked && (
            <span data-worked-tick role="img" aria-label="Worked" style={workedTickStyle}>
              {workedTickIcon}
            </span>
          )}
        </span>
        {yourTurn && <YourTurnIndicator />}
        {(showSource || showStaying) && <StayingIndicator staying={entry.staying} />}
        {showSource && lockedByOther && (
          <span data-lock-flag style={lockFlagStyle}>
            {entry.lockedBy} is editing…
          </span>
        )}
        {showSource &&
          corrections.map((correction) => (
            <span key={correction.field} data-correction={correction.field} style={correctionStyle}>
              Correcting {FIELD_LABELS[correction.field]}: {correction.to ?? "—"} (was{" "}
              {correction.from ?? "—"})
            </span>
          ))}
        {showSource && !entry.optimistic && onSetWorking !== undefined && (
          <WorkingCursor working={entry.working} onSetWorking={() => onSetWorking(entry.key)} />
        )}
        {/* The PUBLIC note — prose the operator wrote FOR
            observers, so it renders on every roster surface with no flag to opt
            into. The STAFF note has no roster render at all and stays in the
            detail modal. Deliberately NOT dimmed or truncated: 0.55 on the muted
            token is already near the AA floor, so "make it quieter to keep the
            row clean" is unavailable. */}
        {entry.publicNote !== null && (
          <span data-public-note style={publicNoteStyle}>
            {entry.publicNote}
          </span>
        )}
        {/* WHICH way in this station came in on. It renders
            in the info cell's sub-line row, not in a column: the roster's fixed
            columns are #/station/source/report/precedence/heard and there is no
            band or mode head to hang it under. The LABEL, never the connection
            id — a UUID says nothing to an operator, and an unresolvable `via`
            says so in words rather than going blank, which would make it
            indistinguishable from a station nobody recorded a way in for. */}
        {entry.viaLabel !== null && (
          <span data-via style={viaStyle}>
            via {entry.viaLabel}
          </span>
        )}
        {/* WHO passed this station's traffic. A SEPARATE
            sub-line from the way in above, never folded into it: one answers
            how the traffic travelled and the other who passed it, and they
            differ often enough that one line cannot carry both. The callsign
            renders as itself — there is nothing to resolve it against, which is
            why there is no label projection here the way `via` has one. */}
        {entry.relayedBy !== null && (
          <span data-relayed-by style={relayedByStyle}>
            relayed by {entry.relayedBy}
          </span>
        )}
      </div>
      {badge && (
        <span style={badgeCellStyle}>
          <SourceBadge source={entry.source} />
        </span>
      )}
      {/* The slot stays open on a report-bearing surface even with no report
          yet (an em-dash, as the mock shows for an unreported station): a
          missing cell would shift every column after it left on that row
          alone, breaking alignment with the head strip. */}
      {(showSource || showReport) && (
        <span
          className="mono"
          data-testid="report-cell"
          style={{ ...reportCellStyle, ...reportStyle }}
        >
          {entry.signalReport ?? "—"}
        </span>
      )}
      {/* The Precedence cell — the traffic count rides alongside it since the
          mock reserves no separate column. Gated on the PAIR of props, never on
          "is this the operator console": the observer surfaces
          pass `showPrecedence` and render this cell too. */}
      {(showSource || showPrecedence) && (
        <span style={precedenceCellStyle}>
          <PrecedenceChip precedence={entry.precedence} />
          {/* The count is not new here, but it was a BARE INTEGER under a head strip that says only
              "Precedence". The operator who typed it can read it; nobody else
              can tell it from a report, a list position or a round number — so
              it is labelled on every surface that shows it, this console
              included. `aria-label` carries the same words for assistive tech,
              phrased as a sentence fragment rather than the terse visible form. */}
          {entry.traffic !== null && (
            <span
              className="mono"
              data-traffic
              aria-label={`Traffic: ${entry.traffic}`}
              style={trafficStyle}
            >
              {entry.traffic} traffic
            </span>
          )}
        </span>
      )}
      <span style={heardCellStyle}>
        {heardAbsolute ? absoluteLocalTime(entry.addedAt) : humanizeTime(entry.addedAt, now)}
      </span>
      {showSource && !entry.optimistic && onEdit !== undefined && (
        <button
          type="button"
          style={editButtonStyle}
          onClick={onEdit}
          disabled={lockedByOther}
          aria-label={`Edit ${entry.callsign}`}
        >
          Edit
        </button>
      )}
    </li>
  );
}
