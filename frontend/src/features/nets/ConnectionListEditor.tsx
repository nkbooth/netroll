// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * The repeating per-kind connection editor a net owner edits their list of
 * ways-to-reach-the-net in. Edit mode only — the sub-resource is
 * keyed on a definition id, which does not exist until create returns.
 *
 * Its own file rather than another thousand lines inside
 * `NetDefinitionFormPage`; the row model and its conversions live beside it in
 * `connectionRows.ts`.
 */

import { useId, useRef } from "react";
import type { CSSProperties, ReactElement } from "react";

import { announce } from "../../ui/a11y/LiveRegion";
import { tokens } from "../../ui/tokens/tokens";
import {
  CONNECTION_KINDS,
  KIND_LABELS,
  type ConnectionKind,
} from "./connectionPresentation";
import {
  FIELD_LABELS,
  MAX_CONNECTIONS,
  newConnectionRow,
  propertiesOf,
  withKind,
  type ConnectionProperty,
  type ConnectionRow,
  type RowError,
} from "./connectionRows";
import { BANDS, DMR_NETWORKS, MODES, TONE_MODES } from "./netEnums";

// A `fieldset`, so the row has a real accessible name from its `legend`. That
// brings the UA defaults with it — a margin, and a `min-inline-size: min-content`
// that stops the grid inside from ever shrinking — so both are overridden here.
const rowStyle: CSSProperties = {
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-3)",
  margin: 0,
  marginBottom: "var(--space-2)",
  minInlineSize: 0,
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-2)",
};

const rowHeadStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  gap: "var(--space-2)",
  flexWrap: "wrap",
};

const gridStyle: CSSProperties = {
  display: "flex",
  flexWrap: "wrap",
  gap: "var(--space-3)",
};

const fieldStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
  flex: "1 1 160px",
  minWidth: 0,
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  textTransform: "uppercase",
  letterSpacing: "0.04em",
  color: "var(--text-muted)",
};

const controlStyle: CSSProperties = {
  padding: "var(--space-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-sm)",
  background: "var(--surface-2)",
  color: "var(--text)",
  font: "inherit",
};

const buttonStyle: CSSProperties = {
  background: "transparent",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-sm)",
  color: "var(--text)",
  padding: "var(--space-1) var(--space-2)",
  fontSize: tokens.typography.meta.fontSize,
  fontFamily: "inherit",
  cursor: "pointer",
};

const noteStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
  fontSize: tokens.typography.meta.fontSize,
  margin: 0,
};

const residueStyle: CSSProperties = {
  border: "1px dashed var(--border)",
  borderRadius: "var(--rounded-sm)",
  padding: "var(--space-2)",
  ...noteStyle,
};

/** A row's per-property input, chosen by the property rather than by the kind
 * so a property common to two kinds is rendered by one branch. */
function PropertyField({
  row,
  property,
  disabled,
  onChange,
  errorId,
  suggestionsId,
}: {
  row: ConnectionRow;
  property: ConnectionProperty;
  disabled: boolean;
  onChange: (value: string) => void;
  /** The id of this row's refusal, when the refusal names THIS property. */
  errorId: string | null;
  /** The `<datalist>` this input offers, when the property has suggestions.
   * Passed in rather than emitted here: this component renders once per ROW
   * and carries no row identity, so a list emitted inside it would ship a
   * duplicate DOM id on every row after the first — and every such input would
   * resolve `list=` to the FIRST row's element. axe-core has no active
   * `duplicate-id` rule for non-ARIA ids, so nothing would catch it. */
  suggestionsId: string | null;
}): ReactElement {
  const label = FIELD_LABELS[property];
  const value = row[property];
  const options =
    property === "band" ? BANDS : property === "mode" ? MODES : property === "toneMode" ? TONE_MODES : null;
  const invalid = errorId !== null;
  return (
    <label style={fieldStyle}>
      <span style={labelStyle}>{label}</span>
      {options === null ? (
        <input
          type="text"
          aria-label={label}
          aria-invalid={invalid || undefined}
          aria-describedby={errorId ?? undefined}
          list={suggestionsId ?? undefined}
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
          style={controlStyle}
        />
      ) : (
        <select
          aria-label={label}
          aria-invalid={invalid || undefined}
          aria-describedby={errorId ?? undefined}
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
          style={controlStyle}
        >
          {property === "toneMode" && <option value="">—</option>}
          {options.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </select>
      )}
    </label>
  );
}

/**
 * The eight-kind picker, under whichever label suits the row it sits on.
 *
 * Option VALUES stay the wire tokens; the option TEXT is what
 * the owner reads, and no owner knows what `urf` or `ysf` mean.
 *
 * `withOther` puts a ninth option on a row that came from `other`, so
 * reclassifying is a door that swings both ways. It is absent everywhere else,
 * which keeps `other` the deliberately awkward choice it is meant to be — and
 * keeps a count of `other` rows honest as the instrument that says whether the
 * closed set is cut wrong.
 */
function KindPicker({
  label,
  value,
  disabled,
  withOther,
  onChange,
}: {
  label: string;
  value: string;
  disabled: boolean;
  withOther: boolean;
  onChange: (kind: ConnectionKind) => void;
}): ReactElement {
  return (
    <label style={fieldStyle}>
      <span style={labelStyle}>{label}</span>
      <select
        aria-label={label}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value as ConnectionKind)}
        style={controlStyle}
      >
        {CONNECTION_KINDS.map((kind) => (
          <option key={kind} value={kind}>
            {KIND_LABELS[kind]}
          </option>
        ))}
        {withOther && <option value="other">{KIND_LABELS.other}</option>}
      </select>
    </label>
  );
}

/**
 * The ordered list of ways a net can be reached, with add, remove and reorder.
 *
 * The array order IS the owner's order and IS the ADIF-export designation —
 * there is no separate control for the latter, because a second source of
 * truth for a fact the array already carries is a second thing to get wrong.
 */
export function ConnectionListEditor({
  rows,
  onChange,
  disabled,
  rowError,
}: {
  rows: readonly ConnectionRow[];
  onChange: (rows: ConnectionRow[]) => void;
  disabled: boolean;
  /** A refusal the server (or the pre-write gate) attributed to one row. */
  rowError: RowError | null;
}): ReactElement {
  const idPrefix = useId();
  // Where focus goes after an operation that destroys the control that started
  // it. Move-up on row 1 disables that same button, so without this the focus
  // ring lands on `document.body` and a keyboard owner is lost in the page.
  const groupRefs = useRef(new Map<number, HTMLFieldSetElement>());
  const focusRow = (index: number): void => {
    // A microtask, not a synchronous call: React flushes a discrete event's
    // updates before microtasks drain, so by the time this runs the list has
    // re-rendered and the ref map holds the fieldset now sitting at `index`.
    queueMicrotask(() => {
      const group = groupRefs.current.get(index);
      // A reorder detaches and reattaches nodes, and a remove leaves the map
      // holding one that is no longer in the document.
      if (group?.isConnected === true) {
        group.focus();
      }
    });
  };

  const replace = (index: number, row: ConnectionRow): void => {
    onChange(rows.map((existing, i) => (i === index ? row : existing)));
  };

  const move = (index: number, to: number): void => {
    if (to < 0 || to >= rows.length) {
      return;
    }
    const next = [...rows];
    const [moved] = next.splice(index, 1);
    next.splice(to, 0, moved);
    onChange(next);
    announce(`Connection moved to position ${to + 1} of ${next.length}.`);
    focusRow(to);
  };

  const remove = (index: number): void => {
    // The domain refuses a net with no way to reach it; refusing here means an
    // owner meets the rule as a disabled control rather than as a 400.
    if (rows.length <= 1) {
      return;
    }
    const next = rows.filter((_, i) => i !== index);
    onChange(next);
    announce(`Connection removed. ${next.length} remaining.`);
    focusRow(Math.max(0, index - 1));
  };

  const add = (kind: ConnectionKind): void => {
    if (rows.length >= MAX_CONNECTIONS) {
      return;
    }
    const next = [...rows, newConnectionRow(kind)];
    onChange(next);
    announce(`Connection added at position ${next.length} of ${next.length}.`);
    focusRow(next.length - 1);
  };

  const full = rows.length >= MAX_CONNECTIONS;

  // ONE element for the whole editor, outside the row loop: `PropertyField`
  // renders per row and has no row identity, so emitting it there would give
  // every DMR row after the first a duplicate id. `useId` keeps two editors on
  // one page apart.
  const networkSuggestionsId = `${idPrefix}-dmr-networks`;

  return (
    <div data-testid="connection-list-editor">
      <datalist id={networkSuggestionsId}>
        {DMR_NETWORKS.map((network) => (
          <option key={network} value={network} />
        ))}
      </datalist>
      {rows.map((row, index) => {
        const onlyOne = rows.length <= 1;
        const error = rowError?.index === index ? rowError : null;
        // One id per row rather than per error, so `aria-describedby` on the
        // control and `id` on the message are written from the same source.
        const errorId = `${idPrefix}-row-${index}-error`;
        return (
          <fieldset
            key={row.id ?? `new-${index}`}
            data-testid="connection-row"
            ref={(node) => {
              // Attach only. React calls a detaching ref with `null`, and
              // across a reorder that null can arrive AFTER the new node for
              // the same index — recording it would erase the target.
              if (node !== null) {
                groupRefs.current.set(index, node);
              }
            }}
            // Not reachable by Tab (the controls inside are); reachable by the
            // programmatic focus an add, a move or a remove performs.
            tabIndex={-1}
            aria-describedby={error !== null ? errorId : undefined}
            style={rowStyle}
          >
            {/* The accessible name of the group. Without it a three-row list
                offers three controls all named "Frequency (MHz)" and three
                buttons all named "Remove", and nothing tells them apart. */}
            <legend style={labelStyle}>
              {`Connection ${index + 1} of ${rows.length} — ${
                KIND_LABELS[row.kind] ?? row.kind
              }`}
            </legend>
            <div style={rowHeadStyle}>
              {index === 0 && (
                <span data-testid="adif-export-note" style={noteStyle}>
                  Logged as the QSO an ADIF export describes. Reorder the list
                  to change it.
                </span>
              )}
              <span style={{ display: "flex", gap: "var(--space-1)" }}>
                <button
                  type="button"
                  onClick={() => move(index, index - 1)}
                  disabled={disabled || index === 0}
                  style={buttonStyle}
                >
                  Move up
                </button>
                <button
                  type="button"
                  onClick={() => move(index, index + 1)}
                  disabled={disabled || index === rows.length - 1}
                  style={buttonStyle}
                >
                  Move down
                </button>
                <button
                  type="button"
                  onClick={() => remove(index)}
                  disabled={disabled || onlyOne}
                  style={buttonStyle}
                >
                  Remove
                </button>
              </span>
            </div>
            {onlyOne && (
              <span style={noteStyle}>
                A net needs at least one way to reach it, so this one cannot be
                removed. Add another first.
              </span>
            )}
            {row.reservedLabel && (
              <div data-testid="unclassified-residue" style={residueStyle}>
                NetRoll could not tell which network this value belongs to, so
                it kept it as written: <strong>{row.detail}</strong>. Pick the
                network below to file it properly.
              </div>
            )}
            <div style={gridStyle}>
              {/* `other` is not one of the eight, so its row selects none of
                  them and the picker is worded as the way OUT of `other`. A
                  row that HAS prose — one that came from `other` — keeps the
                  ninth option, so the reclassification is reversible. */}
              <KindPicker
                label={row.kind === "other" ? "Reclassify as" : "Connection type"}
                value={row.kind === "other" ? "" : row.kind}
                disabled={disabled}
                withOther={row.label !== "" || row.detail !== ""}
                onChange={(kind) => replace(index, withKind(row, kind))}
              />
              {propertiesOf(row.kind).map((property) =>
                property === "label" && row.reservedLabel ? null : (
                  <PropertyField
                    key={property}
                    row={row}
                    property={property}
                    disabled={disabled}
                    suggestionsId={
                      property === "network" ? networkSuggestionsId : null
                    }
                    errorId={
                      error !== null && error.property === property
                        ? errorId
                        : null
                    }
                    onChange={(value) =>
                      replace(index, { ...row, [property]: value })
                    }
                  />
                ),
              )}
            </div>
            {error !== null && (
              <p id={errorId} role="alert" style={errorStyle}>
                {error.message}
              </p>
            )}
          </fieldset>
        );
      })}
      <div style={{ display: "flex", gap: "var(--space-2)", flexWrap: "wrap" }}>
        <button
          type="button"
          onClick={() => add("hf")}
          disabled={disabled || full}
          style={buttonStyle}
        >
          Add connection
        </button>
        <button
          type="button"
          onClick={() => add("other")}
          disabled={disabled || full}
          style={buttonStyle}
        >
          My net uses something the list doesn&apos;t name
        </button>
        {full && (
          <span style={noteStyle}>
            A net can hold {MAX_CONNECTIONS} ways to reach it. Remove one before
            adding another.
          </span>
        )}
      </div>
    </div>
  );
}
