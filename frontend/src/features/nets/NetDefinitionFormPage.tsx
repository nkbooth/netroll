// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useId, useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useNavigate, useParams } from "react-router";

import { messageForProblem } from "../../errors/problemMessages";
import { Breadcrumb } from "../../ui/components/Breadcrumb";
import { Panel } from "../../ui/components/Panel";
import { tokens } from "../../ui/tokens/tokens";
import { absoluteLocalTime } from "../../ui/util/humanizeTime";
import type { Account, Problem } from "../auth/authApi";
import { ProblemError, fetchCurrentAccount } from "../auth/authApi";
import { useAuthRequest } from "../auth/useAuthRequest";
import {
  fetchFormToken,
  withBotMitigation,
} from "../botMitigation/botMitigation";
import {
  addNetOwner,
  clearDeliveryConfig,
  clearNetSchedule,
  createNetDefinition,
  getDeliveryConfig,
  getNetDefinition,
  getNetOccurrences,
  getNetSchedule,
  removeNetOwner,
  setDeliveryConfig,
  setNetConnections,
  setNetSchedule,
  updateNetDefinition,
  type NetDefinition,
  type NetDefinitionFields,
  type Occurrence,
  type Owner,
  type Schedule,
} from "./netsApi";
import { CATEGORIES, NET_TYPES, VISIBILITIES } from "./netEnums";
import { ConnectionListEditor } from "./ConnectionListEditor";
import {
  connectionToRow,
  firstRowRefusal,
  newConnectionRow,
  rfRow,
  rowToInput,
  type ConnectionRow,
  type RowError,
} from "./connectionRows";
import { netFormDestination } from "./netsGate";
import { netPermalink } from "./netPermalink";
import { StartNetControl } from "../session/StartNetControl";

const SCHEDULE_KINDS = ["one-off", "recurring"];
const FREQUENCIES = ["daily", "weekly", "monthly"];
const WEEKDAYS = [
  "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday",
];

/** The browser's IANA timezone, the sensible default for a new schedule. */
const browserTimezone = (): string =>
  Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

/** The timezone options for the schedule select — the full IANA list when the
 * runtime exposes it, else just the browser's own zone. */
function timezoneOptions(): string[] {
  const supported =
    typeof Intl.supportedValuesOf === "function"
      ? Intl.supportedValuesOf("timeZone")
      : [];
  const tz = browserTimezone();
  return supported.length > 0
    ? supported
    : Array.from(new Set([tz, "UTC"]));
}

/** The IANA offset (in ms, UTC minus `timeZone`'s wall clock — negative for
 * zones behind UTC) that applies at the instant `at`, computed from `Intl`
 * only (no date library). */
function offsetMillisAt(at: Date, timeZone: string): number {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone,
    hourCycle: "h23",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).formatToParts(at);
  const get = (type: string): number =>
    Number(parts.find((p) => p.type === type)?.value ?? "0");
  // Reading midnight-hour "24" back as "00" the next day (some locales/tz
  // combinations render the last instant of a day as 24:00).
  const hour = get("hour") % 24;
  const wallAsUtc = Date.UTC(
    get("year"),
    get("month") - 1,
    get("day"),
    hour,
    get("minute"),
    get("second"),
  );
  return wallAsUtc - at.getTime();
}

/** Converts a `datetime-local` input's wall-clock value
 * ("YYYY-MM-DDTHH:MM[:SS]"), interpreted as local time IN `timeZone`, to an
 * RFC 3339 UTC instant string. `datetime-local` never carries an offset —
 * this is the one place that wall-clock-in-a-chosen-zone gets resolved to an
 * absolute instant before it reaches the wire, whose storage contract is a
 * UTC instant. Uses only `Intl` (no date library): a first-guess instant
 * from reading the wall-clock literally as UTC, corrected by the target
 * zone's offset, resolved a second time in case the guess crossed a DST
 * boundary relative to the actual instant. */
function zonedDateTimeToUtcRfc3339(localValue: string, timeZone: string): string {
  if (!localValue) {
    return "";
  }
  const naiveUtcGuess = new Date(
    localValue.length === 16 ? `${localValue}:00Z` : `${localValue}Z`,
  );
  if (Number.isNaN(naiveUtcGuess.getTime())) {
    return localValue;
  }
  const firstPass = new Date(
    naiveUtcGuess.getTime() - offsetMillisAt(naiveUtcGuess, timeZone),
  );
  const resolved = new Date(
    naiveUtcGuess.getTime() - offsetMillisAt(firstPass, timeZone),
  );
  return resolved.toISOString();
}

/** The inverse of [`zonedDateTimeToUtcRfc3339`]: renders an absolute UTC
 * instant as the `datetime-local` wall-clock value ("YYYY-MM-DDTHH:MM") it has
 * IN `timeZone`. Needed to hydrate the one-off control, whose stored form is a
 * UTC instant while its input is zone-local wall clock — without this the
 * panel would show the owner a different clock time than they set. */
function utcRfc3339ToZonedDateTime(iso: string, timeZone: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) {
    return "";
  }
  const wall = new Date(at.getTime() + offsetMillisAt(at, timeZone));
  return wall.toISOString().slice(0, 16);
}

type ScheduleFormState = {
  kind: string;
  timezone: string;
  oneOffStartAt: string;
  frequency: string;
  timeOfDay: string;
  weekday: string;
  dayOfMonth: string;
};

/** The schedule panel's starting values for a net that has none. Also the
 * fallback for fields a stored rule's kind does not carry. */
function scheduleDefaults(): ScheduleFormState {
  return {
    kind: "recurring",
    timezone: browserTimezone(),
    oneOffStartAt: "",
    frequency: "weekly",
    timeOfDay: "20:00",
    weekday: "tuesday",
    dayOfMonth: "1",
  };
}

/** Builds the wire schedule from the schedule form, sending only the fields
 * the chosen kind/frequency needs (the server validates authoritatively). */
function toSchedule(s: ScheduleFormState): Schedule {
  if (s.kind === "one-off") {
    return {
      kind: "one-off",
      timezone: s.timezone,
      oneOffStartAt: zonedDateTimeToUtcRfc3339(s.oneOffStartAt, s.timezone),
    };
  }
  return {
    kind: "recurring",
    timezone: s.timezone,
    frequency: s.frequency,
    timeOfDay: s.timeOfDay,
    ...(s.frequency === "weekly" ? { weekday: s.weekday } : {}),
    ...(s.frequency === "monthly" ? { dayOfMonth: s.dayOfMonth } : {}),
  };
}

/** Projects a STORED wire schedule onto the schedule form —
 * the inverse of [`toSchedule`]. Fields the stored kind does not carry keep
 * `defaults`, so switching kind in the UI still lands on sane values rather
 * than blanks; the one-off instant is converted back to wall clock in the
 * net's OWN timezone, not the browser's. */
function fromSchedule(
  stored: Schedule,
  defaults: ScheduleFormState,
): ScheduleFormState {
  const timezone = stored.timezone || defaults.timezone;
  if (stored.kind === "one-off") {
    return {
      ...defaults,
      kind: "one-off",
      timezone,
      oneOffStartAt:
        stored.oneOffStartAt === undefined
          ? defaults.oneOffStartAt
          : utcRfc3339ToZonedDateTime(stored.oneOffStartAt, timezone),
    };
  }
  return {
    ...defaults,
    kind: "recurring",
    timezone,
    frequency: stored.frequency ?? defaults.frequency,
    timeOfDay: stored.timeOfDay ?? defaults.timeOfDay,
    weekday: stored.weekday ?? defaults.weekday,
    dayOfMonth: stored.dayOfMonth ?? defaults.dayOfMonth,
  };
}

/** The SCALAR fields of a definition — what the create body carries beside its
 * connection list and what the scalar `PUT` sends. No connection fact lives
 * here: frequency, band, mode, offset, tone, node and reflector
 * are each one connection's, held in `connectionRows`. */
type FormState = {
  title: string;
  description: string;
  country: string;
  state: string;
  grid: string;
  netCategory: string;
  netType: string;
  expectedDuration: string;
  visibility: string;
};

const EMPTY_FORM: FormState = {
  title: "",
  description: "",
  country: "",
  state: "",
  grid: "",
  netCategory: "traffic",
  netType: "open",
  expectedDuration: "",
  visibility: "listed",
};

/** Fills the scalar form from a loaded definition; its connections seed
 * `connectionRows` separately. */
function fromDefinition(def: NetDefinition): FormState {
  return {
    title: def.title,
    description: def.description ?? "",
    country: def.country ?? "",
    state: def.state ?? "",
    grid: def.grid ?? "",
    netCategory: def.netCategory,
    netType: def.netType,
    expectedDuration:
      def.expectedDurationMinutes === null
        ? ""
        : String(def.expectedDurationMinutes),
    visibility: def.visibility,
  };
}

// The form keys are exactly the wire field names — the server treats an empty
// optional as "not set", so the whole form is the request body.
const toFields = (form: FormState): NetDefinitionFields => ({ ...form });

/** Capitalizes only the first character — "roll-call" → "Roll-call", "one-off"
 * → "One-off" — for segmented-control/radio-card labels built off wire enum
 * values (never title-cased per hyphen segment; that would read "Roll-Call"). */
function sentenceCase(value: string): string {
  return value.length === 0 ? value : value[0].toUpperCase() + value.slice(1);
}

/** The rail's one-line cadence preview, built from the (possibly unsaved)
 * schedule draft — reads the same `schedule` state the Schedule fieldset
 * edits, so it previews before AND after a save.
 *
 * On the CREATE form the Schedule fieldset doesn't exist yet (the backend's
 * schedule endpoint needs a definition id, so it's edit-only, right after
 * this form's initial "Create net" redirects there) — `schedule` state is
 * just its unset defaults, and showing a cadence built from them read as a
 * real, already-configured schedule when nothing has been set at all
 * (real bug: a brand-new net showed "Weekly · Tuesday 20:00" before the user
 * had ever touched a schedule control). `creating` short-circuits that. */
function cadenceSummary(schedule: ScheduleFormState, creating: boolean): string {
  if (creating) {
    return "Set after creating";
  }
  if (schedule.kind === "one-off") {
    return "One-off";
  }
  const freq = sentenceCase(schedule.frequency);
  if (schedule.frequency === "weekly") {
    return `${freq} · ${sentenceCase(schedule.weekday)} ${schedule.timeOfDay}`;
  }
  if (schedule.frequency === "monthly") {
    return `${freq} · day ${schedule.dayOfMonth} ${schedule.timeOfDay}`;
  }
  return `${freq} · ${schedule.timeOfDay}`;
}

const pageStyle: CSSProperties = {
  maxWidth: "1040px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
  fontSize: tokens.typography.body.fontSize,
  lineHeight: tokens.typography.body.lineHeight,
};

const headingStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  margin: "0 0 var(--space-4)",
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
  flex: "1 1 200px",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
};

const controlStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-3)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
};

const primaryButtonStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-4)",
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const secondaryButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-3)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
  marginTop: "var(--space-3)",
};

/** The schedule panel's "still loading its stored rule" line. The controls are
 * disabled while the load is unresolved; without a visible
 * reason, a request that never settles is indistinguishable from a brief one
 * and reads as a dead panel. */
const schedulePendingStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  margin: 0,
};

// ---- Design-handoff (3a) layout chrome: card body, two-column form + rail ----

const panelStyle: CSSProperties = {
  padding: "var(--space-5)",
  marginBottom: "var(--space-5)",
};

const columnsStyle: CSSProperties = {
  display: "flex",
  flexWrap: "wrap",
  gap: "var(--space-6)",
  alignItems: "flex-start",
};

const formColumnStyle: CSSProperties = {
  flex: "1 1 480px",
  minWidth: 0,
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-5)",
};

const railStyle: CSSProperties = {
  flex: "0 1 300px",
  minWidth: "260px",
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-4)",
  position: "sticky",
  top: "var(--space-6)",
};

const fieldsetStyle: CSSProperties = {
  border: "none",
  padding: 0,
  margin: 0,
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-3)",
};

const legendStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: 800,
  letterSpacing: "0.07em",
  textTransform: "uppercase",
  color: "var(--accent-ink)",
  padding: 0,
  margin: "0 0 var(--space-1)",
};

const railCardStyle: CSSProperties = {
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-lg)",
  padding: "var(--space-4)",
};

const railHeadingStyle: CSSProperties = {
  ...labelStyle,
  marginBottom: "var(--space-3)",
};

const summaryBodyStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-2)",
  fontSize: tokens.typography.meta.fontSize,
};

const summaryRowStyle: CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  gap: "var(--space-2)",
};

const summaryValueStyle: CSSProperties = { color: "var(--text)" };

const actionStackStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-2)",
};

const startSessionCaptionStyle: CSSProperties = {
  ...labelStyle,
  marginBottom: "var(--space-1)",
};

const cancelButtonStyle: CSSProperties = {
  background: "transparent",
  border: "none",
  color: "var(--text-muted)",
  padding: "var(--space-1)",
  fontWeight: 700,
  fontSize: tokens.typography.meta.fontSize,
  fontFamily: "inherit",
  cursor: "pointer",
};

// ---- Segmented control (pill row) — Net type + Schedule kind (task 89) ----

interface SegmentOption {
  readonly value: string;
  readonly label: string;
}

const segmentGroupStyle: CSSProperties = {
  display: "flex",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  overflow: "hidden",
  width: "fit-content",
};

const segmentButtonBaseStyle: CSSProperties = {
  flex: "1 1 auto",
  textAlign: "center",
  padding: "var(--space-2) var(--space-3)",
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: 700,
  fontFamily: "inherit",
  border: "none",
  cursor: "pointer",
};

const segmentButtonActiveStyle: CSSProperties = {
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  fontWeight: 800,
};

const segmentButtonInactiveStyle: CSSProperties = {
  background: "var(--surface-2)",
  color: "var(--text-muted)",
};

/** Pill-style segmented control for a single-select, small option set (Net
 * type, Schedule kind) — same underlying string state as the `<select>` it
 * replaces, just a click-to-choose row of `role="radio"` buttons instead of
 * a dropdown. */
function SegmentedControl({
  legend,
  value,
  options,
  onChange,
  disabled,
}: {
  legend: string;
  value: string;
  options: readonly SegmentOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
}): ReactElement {
  return (
    <div role="radiogroup" aria-label={legend} style={segmentGroupStyle}>
      {options.map((option, index) => {
        const checked = value === option.value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={checked}
            disabled={disabled}
            onClick={() => onChange(option.value)}
            style={{
              ...segmentButtonBaseStyle,
              ...(checked ? segmentButtonActiveStyle : segmentButtonInactiveStyle),
              ...(index > 0 ? { borderLeft: "1px solid var(--border)" } : {}),
            }}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}

// ---- Visibility radio-cards (task 90) ----

const VISIBILITY_DESCRIPTIONS: Record<string, string> = {
  // The consequence used to sit on the `unlisted` card, which a reader only meets
  // AFTER choosing unlisted — but the publishing happens the moment `listed` is
  // chosen, so the warning belonged on the choice that causes it. Neither
  // string is pinned by a test: prose assertions breach the testing contract.
  listed:
    "Shows on Discovery and in search. Discovery links the title to this net's permalink, so listing publishes its link token to anyone browsing.",
  unlisted:
    "Reachable only by its link token. Links already shared keep working — including any picked up from Discovery while this net was listed.",
};

const visibilityCardBaseStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  alignItems: "flex-start",
  width: "100%",
  padding: "var(--space-3)",
  borderRadius: "var(--rounded-md)",
  background: "transparent",
  fontFamily: "inherit",
  textAlign: "left",
  cursor: "pointer",
};

const visibilityCardActiveStyle: CSSProperties = {
  border: "1px solid var(--accent)",
  background: "color-mix(in srgb, var(--accent) 8%, transparent)",
};

const visibilityCardInactiveStyle: CSSProperties = {
  border: "1px solid var(--border)",
};

const visibilityTitleStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
  color: "var(--text)",
};

const visibilityDescStyle: CSSProperties = {
  display: "block",
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  marginTop: "2px",
};

function visibilityDotStyle(checked: boolean): CSSProperties {
  return {
    width: "14px",
    height: "14px",
    borderRadius: "var(--rounded-full)",
    border: checked ? "4px solid var(--accent)" : "2px solid var(--border)",
    flex: "0 0 auto",
    marginTop: "2px",
  };
}

/** One Listed/Unlisted radio-card — same `form.visibility` state the old
 * `<select>` drove, restyled per the mock's sticky-rail picker. The visible
 * title is the accessible NAME (`aria-label`); the longer description is
 * exposed separately via `aria-describedby` rather than folded into the name
 * (WCAG 2.5.3 Label in Name — the name must still literally match the
 * visible title, which a name built from all child text would not). */
function VisibilityCard({
  value,
  checked,
  onSelect,
  disabled,
}: {
  value: string;
  checked: boolean;
  onSelect: () => void;
  disabled?: boolean;
}): ReactElement {
  const descId = useId();
  return (
    <button
      type="button"
      role="radio"
      aria-checked={checked}
      aria-label={sentenceCase(value)}
      aria-describedby={descId}
      disabled={disabled}
      onClick={onSelect}
      style={{
        ...visibilityCardBaseStyle,
        ...(checked ? visibilityCardActiveStyle : visibilityCardInactiveStyle),
      }}
    >
      <span aria-hidden="true" style={visibilityDotStyle(checked)} />
      <span>
        <span style={visibilityTitleStyle}>{sentenceCase(value)}</span>
        <span id={descId} style={visibilityDescStyle}>
          {VISIBILITY_DESCRIPTIONS[value] ?? ""}
        </span>
      </span>
    </button>
  );
}

// ---- Owner chip row (task 91) ----

const chipRowStyle: CSSProperties = {
  listStyle: "none",
  padding: 0,
  margin: 0,
  display: "flex",
  flexWrap: "wrap",
  gap: "var(--space-2)",
  alignItems: "center",
};

const chipStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-1) var(--space-3)",
  fontSize: tokens.typography.meta.fontSize,
};

const youBadgeStyle: CSSProperties = {
  fontSize: tokens.typography.microCaps.fontSize,
  fontWeight: 800,
  textTransform: "uppercase",
  letterSpacing: "0.04em",
  color: "var(--accent-ink)",
  background: "var(--self-fill)",
  border: "1px solid var(--self-border)",
  borderRadius: "5px",
  padding: "1px 6px",
};

const removeChipButtonStyle: CSSProperties = {
  background: "transparent",
  border: "none",
  color: "var(--text-muted)",
  cursor: "pointer",
  fontWeight: 700,
  fontSize: tokens.typography.body.fontSize,
  lineHeight: 1,
  padding: 0,
};

const addOwnerRowStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  alignItems: "center",
};

const addOwnerButtonStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
  background: "transparent",
  border: "1px dashed var(--border)",
  color: "var(--accent-ink)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-3)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
  fontFamily: "inherit",
  cursor: "pointer",
};

/**
 * Create/edit surface for a net definition. One component serves
 * both `/nets/new` (create) and `/nets/:id/edit` (edit, prefilled). Gated on
 * consent + callsign (net creation is the first callsign-gated action) —
 * a signed-out/unconsented/callsign-less visitor is redirected. The server's
 * field-level `detail` is what surfaces a specific validation message.
 *
 * Layout follows the design handoff's "3a" spec: a bordered card holding a
 * fieldset-grouped form (Identity/Operating/Geography/Schedule/Owners) beside
 * a sticky rail (visibility radio-cards, a live summary, and the Save/Cancel
 * action stack) — real product surface not shown in the mock (delivery
 * config, share link, reveal-once secret, occurrences) stays intact below.
 */
export function NetDefinitionFormPage(): ReactElement {
  const navigate = useNavigate();
  const { id } = useParams();

  // undefined = still loading /me; null = signed out.
  const [account, setAccount] = useState<Account | null | undefined>(undefined);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [version, setVersion] = useState<number | null>(null);
  // The loaded net's stable permalink token — drives the read-only share link
  // shown in edit mode. Null until an edit-mode definition is loaded.
  const [linkToken, setLinkToken] = useState<string | null>(null);
  // The loaded net's owners — drives the edit-mode owner panel.
  const [owners, setOwners] = useState<Owner[]>([]);
  const [addCallsign, setAddCallsign] = useState("");
  const [ownerBusy, setOwnerBusy] = useState(false);
  // An owner add/remove failure (e.g. /errors/last-owner) — surfaced via the
  // mapped alert message, never a hardcoded string.
  const [ownerError, setOwnerError] = useState<Problem | undefined | null>(
    null,
  );
  // Set when the initial edit-mode load fails (403/404/network) — surfaced
  // immediately rather than left for the user to discover via a blank form.
  const [loadError, setLoadError] = useState<{ problem?: Problem } | null>(
    null,
  );
  // The connection list — one row per way to reach the net, in
  // the owner's order. In EDIT mode it is seeded from the loaded definition and
  // written back as a full replace, so the array order IS the stored order. In
  // CREATE mode it starts as one blank HF row: a net is born with
  // the ways in its owner declares, and the server refuses a create with none.
  const [connectionRows, setConnectionRows] = useState<ConnectionRow[]>(() =>
    id === undefined ? [newConnectionRow("hf")] : [],
  );
  // Whether THIS net's connections have arrived. The edit route registers one
  // element for `nets/:id/edit` with no `key` (`router.tsx`), so React reuses
  // this component instance across an id change and state survives it — and
  // the connections PUT is a FULL REPLACE keyed on ids, so saving net A's rows
  // against net B would replace B's whole list with copies of A's. Same reason
  // `scheduleLoaded` exists, same shape of fix, and it gates Save rather than
  // only rendering a message. Create has nothing to load.
  const [connectionsLoaded, setConnectionsLoaded] = useState(id === undefined);
  // A refusal the server (or the pre-write gate) attributed to ONE row. While
  // it is set the form head stays clear: a "frequency is required" against a
  // form with three connections is unactionable.
  const [connectionRowError, setConnectionRowError] = useState<RowError | null>(
    null,
  );
  // Schedule sub-resource (edit mode only).
  const [schedule, setSchedule] = useState<ScheduleFormState>(scheduleDefaults);
  // Whether the stored rule has been RESOLVED (loaded, or confirmed absent).
  // The panel is a FULL REPLACE, so submitting it before this settles would
  // overwrite the real rule with these defaults. It is
  // set in every terminal branch of the load, including failure, so a blip
  // cannot lock an owner out of their own schedule.
  const [scheduleLoaded, setScheduleLoaded] = useState(false);
  const [occurrences, setOccurrences] = useState<Occurrence[]>([]);
  const [scheduleBusy, setScheduleBusy] = useState(false);
  const [scheduleError, setScheduleError] = useState<Problem | undefined | null>(
    null,
  );
  // Delivery config sub-resource (edit mode only).
  const [deliveryEmails, setDeliveryEmails] = useState<string[]>([]);
  const [webhookUrl, setWebhookUrl] = useState("");
  // The Discord channel webhook. Held and rendered exactly like
  // `webhookUrl` — same visibility, no masking, no write-only flag — because two
  // adjacent URL fields with different read semantics is how the next author
  // gets one of them wrong.
  const [discordWebhookUrl, setDiscordWebhookUrl] = useState("");
  const [webhookSecretSet, setWebhookSecretSet] = useState(false);
  // The reveal-once minted secret — non-null only immediately after a save that
  // minted one, so the owner can copy it. Never re-fetched.
  const [revealedSecret, setRevealedSecret] = useState<string | null>(null);
  const [deliveryBusy, setDeliveryBusy] = useState(false);
  const [deliveryError, setDeliveryError] = useState<Problem | undefined | null>(
    null,
  );
  // Bot-mitigation state for the CREATE flow: a form token fetched
  // on mount and an always-empty honeypot a real user never fills. Ignored on
  // edit (no create side effect to protect). `undefined` distinctly means "the
  // mount fetch hasn't resolved yet" (gates the create button below), separate
  // from a resolved `null` (mitigation disabled) — a fast submit racing ahead
  // of the mount fetch would otherwise read as a missing token, which the
  // server treats as a bot.
  const [formToken, setFormToken] = useState<string | null | undefined>(
    undefined,
  );
  const [honeypot, setHoneypot] = useState("");
  useEffect(() => {
    if (id === undefined) {
      void fetchFormToken().then(setFormToken);
    }
  }, [id]);
  // (`creating` — id === undefined — isn't declared until later in this
  // component; re-derive it locally rather than reorder unrelated code.)
  const formTokenLoading = id === undefined && formToken === undefined;

  useEffect(() => {
    let cancelled = false;
    fetchCurrentAccount()
      .then((result) => {
        if (!cancelled) {
          setAccount(result);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setAccount(null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (account === undefined) {
      return;
    }
    const destination = netFormDestination(account);
    if (destination !== null) {
      void navigate(destination, { replace: true });
    }
  }, [account, navigate]);

  // Load the definition being edited once the account is known and allowed
  // through — an owner-scoped GET the server authorizes.
  useEffect(() => {
    if (id === undefined || account === undefined || account === null) {
      return;
    }
    if (netFormDestination(account) !== null) {
      return;
    }
    let cancelled = false;
    setLoadError(null);
    // Cleared BEFORE the fetch, not merged into its result. See
    // `connectionsLoaded` above: the rows carry net A's connection IDS, and
    // `into_identified` treats an id it has never seen as a new connection —
    // so a save in the window between an id change and this GET resolving (or
    // forever, if it fails) would replace net B's list with copies of A's.
    setConnectionsLoaded(false);
    setConnectionRows([]);
    setConnectionRowError(null);
    setVersion(null);
    getNetDefinition(id)
      .then((def) => {
        if (!cancelled) {
          setForm(fromDefinition(def));
          setConnectionRows(def.connections.map(connectionToRow));
          setConnectionsLoaded(true);
          setVersion(def.definitionVersion);
          setLinkToken(def.linkToken);
          setOwners(def.owners);
        }
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setLoadError({
            problem: error instanceof ProblemError ? error.problem : undefined,
          });
        }
      });
    // Load the net's upcoming occurrences alongside the definition (best
    // effort — a failure here just leaves the list empty, not the whole form).
    getNetOccurrences(id)
      .then((list) => {
        if (!cancelled) {
          setOccurrences(list);
        }
      })
      .catch(() => {});
    // Load the STORED schedule rule alongside the definition.
    // Best effort like its siblings — but `scheduleLoaded` is set on EVERY
    // outcome, because leaving it false would make the panel permanently
    // unsavable after a transient failure.
    //
    // Hydration is seeded from `scheduleDefaults()`, NOT from the current
    // state, and a `204` resets to those defaults rather than leaving what is
    // already there. The edit route registers one element for `nets/:id/edit`
    // with no `key` (`router.tsx`), so React reuses this component instance
    // across an id change and state survives it. Carrying the previous net's
    // rule into a net that has none would hand the owner a savable panel
    // describing a DIFFERENT net — and the PUT is a full replace, so one click
    // would write net A's schedule onto net B. That is the same failure this
    // story exists to fix, so the load must not depend on prior state at all.
    setScheduleLoaded(false);
    getNetSchedule(id)
      .then((stored) => {
        if (!cancelled) {
          setSchedule(
            stored === null
              ? scheduleDefaults()
              : fromSchedule(stored, scheduleDefaults()),
          );
          setScheduleLoaded(true);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setScheduleLoaded(true);
        }
      });
    // Load the delivery config alongside the definition (best effort). The
    // plaintext secret is never in this read — only whether one is set.
    getDeliveryConfig(id)
      .then((config) => {
        if (!cancelled) {
          setDeliveryEmails(config.emails);
          setWebhookUrl(config.webhookUrl ?? "");
          setDiscordWebhookUrl(config.discordWebhookUrl ?? "");
          setWebhookSecretSet(config.webhookSecretSet);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [id, account]);

  /**
   * Folds a freshly read definition back into the form without throwing away
   * what the owner has typed — the recovery half of a lost race on the
   * connection list.
   *
   * Their edits to a connection they already have are kept, IN THEIR ORDER —
   * a reorder is an edit like any other; a connection that appeared under them
   * is appended; one that was deleted under them goes. Adopting the server's
   * list wholesale would discard their work, and keeping only theirs is
   * exactly the full-replace deletion this guards against.
   */
  const reapplyOnto = (fresh: NetDefinition): void => {
    setVersion(fresh.definitionVersion);
    setConnectionRows((local) => {
      const live = new Set(fresh.connections.map((connection) => connection.id));
      // Rows the owner added in this sitting carry no id, so nothing on the
      // server can correspond to them and they survive by definition.
      const kept = local.filter((row) => row.id === null || live.has(row.id));
      const mine = new Set(kept.map((row) => row.id));
      const arrived = fresh.connections
        .filter((connection) => !mine.has(connection.id))
        .map(connectionToRow);
      return [...kept, ...arrived];
    });
    setConnectionRowError(null);
  };

  /**
   * Places a connection-write refusal where the owner can act on it: against
   * the row the server named, or — for a lost race, the `409` the list's
   * compare-and-swap raises — by re-reading and reapplying, so their next save
   * lands without a page reload and without retyping the list. This once also
   * had to cover a save that had half landed (scalar written, list not); a
   * scalar write no longer touches a connection, so there is no half-landed
   * state left to recover from.
   */
  // A refusal the server pinned to one entry (`connectionIndex`) belongs on
  // that row, not at the form head — on create as on edit, since `POST`
  // refuses a connection through the same problem shape the
  // sub-resource sends. Returns whether the problem was placed.
  const placeConnectionRefusal = (problem: Problem): boolean => {
    if (problem.connectionIndex === undefined) {
      return false;
    }
    setConnectionRowError({
      index: problem.connectionIndex,
      message: messageForProblem(problem),
    });
    return true;
  };

  const reconcileConnectionsFailure = async (
    definitionId: string,
    problem: Problem,
  ): Promise<void> => {
    if (placeConnectionRefusal(problem)) {
      return;
    }
    if (problem.status === 409) {
      try {
        reapplyOnto(await getNetDefinition(definitionId));
      } catch {
        // The re-read is a courtesy, not the outcome. Letting its failure
        // escape would replace the conflict the owner must act on with the
        // read's own problem, and the `throw error` below would never run.
      }
    }
  };

  const { state, run, reset } = useAuthRequest<NetDefinition>(async () => {
    const connections = connectionRows.map(rowToInput);
    if (id === undefined) {
      // A net is born with its ways in: the same per-connection shape the edit
      // page writes, in one request.
      try {
        return await createNetDefinition(
          { ...toFields(form), connections },
          withBotMitigation({}, formToken ?? null, honeypot),
        );
      } catch (error: unknown) {
        if (error instanceof ProblemError) {
          placeConnectionRefusal(error.problem);
        }
        throw error;
      }
    }
    // `connectionsLoaded` gates Save and the load sets `version` beside it, so
    // a null here is a programming error rather than a state to recover from.
    if (version === null) {
      throw new Error("the definition version must be loaded before a save");
    }
    // Two sub-resources, each complete on its own. The ORDER stopped being
    // load-bearing: the scalar request carries no connection
    // fact, so it can neither mint a transient RF row nor delete one, and
    // nothing has to run last to clean up after it. The list goes first for a
    // different reason — its compare-and-swap is aimed at the version this
    // page LOADED, which is the only version that catches a co-owner's edit
    // made while the page was open (a new connection of theirs, absent from
    // rows loaded before they added it, would otherwise be deleted by an owner
    // doing nothing but fixing a typo in the title). The scalar write stays
    // last-write-wins by design and follows.
    try {
      const list = await setNetConnections(id, version, connections);
      setVersion(list.definitionVersion);
    } catch (error: unknown) {
      if (error instanceof ProblemError) {
        await reconcileConnectionsFailure(id, error.problem);
      }
      throw error;
    }
    return updateNetDefinition(id, toFields(form));
  });

  useEffect(() => {
    if (state.status === "success") {
      const def = state.data;
      if (id === undefined) {
        // Created — go to the edit route for the new id.
        void navigate(`/nets/${def.id}/edit`, { replace: true });
      } else {
        setForm(fromDefinition(def));
        setConnectionRows(def.connections.map(connectionToRow));
        setConnectionsLoaded(true);
        setVersion(def.definitionVersion);
        setLinkToken(def.linkToken);
        setOwners(def.owners);
      }
    }
  }, [state, id, navigate]);

  if (account === undefined || netFormDestination(account) !== null) {
    // Redirect effect is in flight; render nothing rather than a flash.
    return <main style={pageStyle} />;
  }

  const set = (key: keyof FormState) => (value: string) =>
    setForm((prev) => ({ ...prev, [key]: value }));

  const textField = (label: string, key: keyof FormState) => (
    <label style={fieldStyle}>
      <span style={labelStyle}>{label}</span>
      <input
        type="text"
        aria-label={label}
        value={form[key]}
        onChange={(event) => set(key)(event.target.value)}
        disabled={state.status === "loading"}
        style={controlStyle}
      />
    </label>
  );

  /** A multi-line field for prose (the net's description): same chrome as
   * `textField`, but tall enough to read what you are typing. */
  const textAreaField = (label: string, key: keyof FormState, rows = 4) => (
    <label style={{ ...fieldStyle, flexBasis: "100%" }}>
      <span style={labelStyle}>{label}</span>
      <textarea
        aria-label={label}
        rows={rows}
        value={form[key]}
        onChange={(event) => set(key)(event.target.value)}
        disabled={state.status === "loading"}
        style={{ ...controlStyle, resize: "vertical", minHeight: "88px" }}
      />
    </label>
  );

  const selectField = (
    label: string,
    key: keyof FormState,
    options: string[],
    includeBlank = false,
  ) => (
    <label style={fieldStyle}>
      <span style={labelStyle}>{label}</span>
      <select
        aria-label={label}
        value={form[key]}
        onChange={(event) => set(key)(event.target.value)}
        disabled={state.status === "loading"}
        style={controlStyle}
      >
        {includeBlank && <option value="">—</option>}
        {options.map((option) => (
          <option key={option} value={option}>
            {option}
          </option>
        ))}
      </select>
    </label>
  );

  const creating = id === undefined;

  /**
   * Runs the pre-write gate, then the save. A row the owner has not finished is
   * refused here, against the row, rather than sent to the server to be refused
   * against an index — on create and on edit alike, since both carry the list.
   */
  const handleSave = (): void => {
    setConnectionRowError(null);
    const refusal = firstRowRefusal(connectionRows);
    if (refusal !== null) {
      setConnectionRowError(refusal);
      return;
    }
    void run();
  };

  // Owner management (edit mode only). Both reload the owner set on
  // success; a refusal (e.g. /errors/last-owner) surfaces via the mapped alert.
  const reloadOwners = async () => {
    if (id === undefined) {
      return;
    }
    const def = await getNetDefinition(id);
    setOwners(def.owners);
  };

  const runOwnerOp = async (op: () => Promise<unknown>) => {
    if (id === undefined) {
      return;
    }
    setOwnerError(null);
    setOwnerBusy(true);
    try {
      await op();
      await reloadOwners();
    } catch (error: unknown) {
      setOwnerError(error instanceof ProblemError ? error.problem : undefined);
    } finally {
      setOwnerBusy(false);
    }
  };

  const handleAddOwner = () =>
    void runOwnerOp(async () => {
      await addNetOwner(id as string, addCallsign);
      setAddCallsign("");
    });

  const handleRemoveOwner = (accountId: string) =>
    void runOwnerOp(() => removeNetOwner(id as string, accountId));

  const setSchedField = (key: keyof ScheduleFormState) => (value: string) =>
    setSchedule((prev) => ({ ...prev, [key]: value }));

  // Save/clear the schedule (edit mode only), reloading occurrences on success.
  // A refusal (e.g. /errors/schedule-invalid) surfaces via the mapped alert.
  const handleSaveSchedule = () =>
    void (async () => {
      if (id === undefined) {
        return;
      }
      setScheduleError(null);
      setScheduleBusy(true);
      try {
        const list = await setNetSchedule(id, toSchedule(schedule));
        setOccurrences(list);
      } catch (error: unknown) {
        setScheduleError(
          error instanceof ProblemError ? error.problem : undefined,
        );
      } finally {
        setScheduleBusy(false);
      }
    })();

  const handleClearSchedule = () =>
    void (async () => {
      if (id === undefined) {
        return;
      }
      setScheduleError(null);
      setScheduleBusy(true);
      try {
        await clearNetSchedule(id);
        setOccurrences([]);
      } catch (error: unknown) {
        setScheduleError(
          error instanceof ProblemError ? error.problem : undefined,
        );
      } finally {
        setScheduleBusy(false);
      }
    })();

  // Delivery config (edit mode only). One email input per entry;
  // add/remove mutate the list. A saved webhook that mints a secret surfaces
  // the reveal-once panel; a config-time refusal surfaces via the mapped alert.
  const setEmailAt = (index: number) => (value: string) =>
    setDeliveryEmails((prev) => prev.map((e, i) => (i === index ? value : e)));
  const addEmail = () => setDeliveryEmails((prev) => [...prev, ""]);
  const removeEmailAt = (index: number) =>
    setDeliveryEmails((prev) => prev.filter((_, i) => i !== index));

  const handleSaveDelivery = () =>
    void (async () => {
      if (id === undefined) {
        return;
      }
      setDeliveryError(null);
      setRevealedSecret(null);
      setDeliveryBusy(true);
      try {
        const trimmedUrl = webhookUrl.trim();
        const trimmedDiscordUrl = discordWebhookUrl.trim();
        const result = await setDeliveryConfig(id, {
          emails: deliveryEmails.map((e) => e.trim()).filter((e) => e !== ""),
          webhookUrl: trimmedUrl === "" ? null : trimmedUrl,
          discordWebhookUrl:
            trimmedDiscordUrl === "" ? null : trimmedDiscordUrl,
        });
        setDeliveryEmails(result.emails);
        setWebhookUrl(result.webhookUrl ?? "");
        setDiscordWebhookUrl(result.discordWebhookUrl ?? "");
        setWebhookSecretSet(result.webhookSecretSet);
        // Present ONLY when this save minted a fresh secret — reveal it once.
        if (result.webhookSecret !== undefined) {
          setRevealedSecret(result.webhookSecret);
        }
      } catch (error: unknown) {
        setDeliveryError(
          error instanceof ProblemError ? error.problem : undefined,
        );
      } finally {
        setDeliveryBusy(false);
      }
    })();

  const handleClearDelivery = () =>
    void (async () => {
      if (id === undefined) {
        return;
      }
      setDeliveryError(null);
      setRevealedSecret(null);
      setDeliveryBusy(true);
      try {
        await clearDeliveryConfig(id);
        setDeliveryEmails([]);
        setWebhookUrl("");
        setDiscordWebhookUrl("");
        setWebhookSecretSet(false);
      } catch (error: unknown) {
        setDeliveryError(
          error instanceof ProblemError ? error.problem : undefined,
        );
      } finally {
        setDeliveryBusy(false);
      }
    })();

  // Cancel just returns the visitor to wherever they came from (My Nets, a
  // net's edit page, etc.) — there is no unsaved-draft concept to discard.
  const handleCancel = () => navigate(-1);

  const busy = state.status === "loading";
  // Edit mode cannot save until THIS net's connections have arrived: the write
  // is a full replace, and an empty (or another net's) list is not a statement
  // the owner has made.
  const saveBlocked = busy || formTokenLoading || (!creating && !connectionsLoaded);
  const currentPageLabel = creating ? "New net" : "Edit net";

  const summaryRfRow = rfRow(connectionRows);
  const summaryRf = {
    frequency: summaryRfRow?.plannedFrequency ?? null,
    bandMode:
      summaryRfRow === null
        ? null
        : `${summaryRfRow.band} · ${summaryRfRow.mode}`,
  };

  return (
    <main style={pageStyle}>
      <Breadcrumb
        items={[
          { label: "Nets", href: "/" },
          { label: "My Nets", href: "/my-nets" },
          { label: currentPageLabel },
        ]}
      />
      {!creating && version !== null && (
        <p style={labelStyle}>Version {version}</p>
      )}
      {loadError && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(loadError.problem)}
        </p>
      )}

      <Panel style={panelStyle}>
        <div style={columnsStyle}>
          <form
            style={formColumnStyle}
            onSubmit={(event) => event.preventDefault()}
          >
            <h1 style={headingStyle}>{currentPageLabel}</h1>

            <fieldset style={fieldsetStyle}>
              <legend style={legendStyle}>Identity</legend>
              {textField("Title", "title")}
              {textAreaField("Description", "description")}
              <div style={gridStyle}>
                {selectField("Category", "netCategory", CATEGORIES)}
                <div style={fieldStyle}>
                  <span style={labelStyle}>Type</span>
                  <SegmentedControl
                    legend="Type"
                    value={form.netType}
                    onChange={set("netType")}
                    options={NET_TYPES.map((value) => ({
                      value,
                      label: sentenceCase(value),
                    }))}
                    disabled={busy}
                  />
                </div>
              </div>
            </fieldset>

            <fieldset style={fieldsetStyle}>
              <legend style={legendStyle}>Operating</legend>
              {textField("Expected duration (minutes)", "expectedDuration")}
            </fieldset>

            {/* Frequency, band, mode and the repeater details are each ONE
                connection's facts, so they are typed per connection here — on
                create as on edit. A second, flat place to type
                them would be a second source of truth for the same fact. */}
            <fieldset style={fieldsetStyle}>
              <legend style={legendStyle}>Ways to reach this net</legend>
              <ConnectionListEditor
                rows={connectionRows}
                onChange={(next) => {
                  setConnectionRows(next);
                  // The refusal is attributed to a POSITION, which is all the
                  // server can say. Any change to the list makes that
                  // position a different connection, so the message goes
                  // rather than re-attaching to one it was never about — and
                  // the failed attempt it came from goes with it, or the
                  // head alert it was suppressing would surface in its place.
                  if (connectionRowError !== null) {
                    setConnectionRowError(null);
                    reset();
                  }
                }}
                disabled={busy}
                rowError={connectionRowError}
              />
            </fieldset>

            <fieldset style={fieldsetStyle}>
              <legend style={legendStyle}>Geography</legend>
              <div style={gridStyle}>
                {textField("Country", "country")}
                {textField("State", "state")}
                {textField("Grid square", "grid")}
              </div>
            </fieldset>

            {!creating && (
              <fieldset style={fieldsetStyle}>
                <legend style={legendStyle}>Schedule</legend>
                <div style={fieldStyle}>
                  <span style={labelStyle}>Schedule kind</span>
                  <SegmentedControl
                    legend="Schedule kind"
                    value={schedule.kind}
                    onChange={setSchedField("kind")}
                    options={SCHEDULE_KINDS.map((value) => ({
                      value,
                      label: sentenceCase(value),
                    }))}
                    disabled={scheduleBusy}
                  />
                </div>
                <div style={gridStyle}>
                  <label style={fieldStyle}>
                    <span style={labelStyle}>Timezone</span>
                    <select
                      aria-label="Timezone"
                      value={schedule.timezone}
                      onChange={(event) =>
                        setSchedField("timezone")(event.target.value)
                      }
                      disabled={scheduleBusy}
                      style={controlStyle}
                    >
                      {timezoneOptions().map((tz) => (
                        <option key={tz} value={tz}>
                          {tz}
                        </option>
                      ))}
                    </select>
                  </label>
                  {schedule.kind === "one-off" && (
                    <label style={fieldStyle}>
                      <span style={labelStyle}>Start (local date &amp; time)</span>
                      <input
                        type="datetime-local"
                        aria-label="One-off start"
                        value={schedule.oneOffStartAt}
                        onChange={(event) =>
                          setSchedField("oneOffStartAt")(event.target.value)
                        }
                        disabled={scheduleBusy}
                        style={controlStyle}
                      />
                    </label>
                  )}
                  {schedule.kind === "recurring" && (
                    <>
                      <label style={fieldStyle}>
                        <span style={labelStyle}>Frequency</span>
                        <select
                          aria-label="Frequency"
                          value={schedule.frequency}
                          onChange={(event) =>
                            setSchedField("frequency")(event.target.value)
                          }
                          disabled={scheduleBusy}
                          style={controlStyle}
                        >
                          {FREQUENCIES.map((option) => (
                            <option key={option} value={option}>
                              {option}
                            </option>
                          ))}
                        </select>
                      </label>
                      <label style={fieldStyle}>
                        <span style={labelStyle}>Time of day (HH:MM)</span>
                        <input
                          type="time"
                          aria-label="Time of day"
                          value={schedule.timeOfDay}
                          onChange={(event) =>
                            setSchedField("timeOfDay")(event.target.value)
                          }
                          disabled={scheduleBusy}
                          style={controlStyle}
                        />
                      </label>
                      {schedule.frequency === "weekly" && (
                        <label style={fieldStyle}>
                          <span style={labelStyle}>Weekday</span>
                          <select
                            aria-label="Weekday"
                            value={schedule.weekday}
                            onChange={(event) =>
                              setSchedField("weekday")(event.target.value)
                            }
                            disabled={scheduleBusy}
                            style={controlStyle}
                          >
                            {WEEKDAYS.map((option) => (
                              <option key={option} value={option}>
                                {option}
                              </option>
                            ))}
                          </select>
                        </label>
                      )}
                      {schedule.frequency === "monthly" && (
                        <label style={fieldStyle}>
                          <span style={labelStyle}>Day of month</span>
                          <input
                            type="number"
                            min={1}
                            max={31}
                            aria-label="Day of month"
                            value={schedule.dayOfMonth}
                            onChange={(event) =>
                              setSchedField("dayOfMonth")(event.target.value)
                            }
                            disabled={scheduleBusy}
                            style={controlStyle}
                          />
                        </label>
                      )}
                    </>
                  )}
                </div>
                <div style={{ display: "flex", gap: "var(--space-2)" }}>
                  <button
                    type="button"
                    onClick={handleSaveSchedule}
                    disabled={scheduleBusy || !scheduleLoaded}
                    style={primaryButtonStyle}
                  >
                    Save schedule
                  </button>
                  <button
                    type="button"
                    onClick={handleClearSchedule}
                    disabled={scheduleBusy || !scheduleLoaded}
                    style={secondaryButtonStyle}
                  >
                    Clear schedule
                  </button>
                </div>
                {!scheduleLoaded && (
                  <p role="status" style={schedulePendingStyle}>
                    Loading this net's saved schedule…
                  </p>
                )}
                {scheduleError !== null && (
                  <p role="alert" style={errorStyle}>
                    {messageForProblem(scheduleError)}
                  </p>
                )}
                <h3 style={{ ...labelStyle, margin: 0 }}>
                  Upcoming occurrences
                </h3>
                <ul
                  aria-label="Upcoming occurrences"
                  style={{ listStyle: "none", padding: 0, margin: 0 }}
                >
                  {occurrences.map((occ) => (
                    <li key={occ.id}>{absoluteLocalTime(occ.scheduledStartAt)}</li>
                  ))}
                </ul>
              </fieldset>
            )}

            {!creating && (
              <fieldset style={fieldsetStyle}>
                <legend style={legendStyle}>Owners</legend>
                <ul style={chipRowStyle}>
                  {owners.map((owner) => {
                    const label = owner.callsign ?? owner.accountId;
                    const isSelf = owner.accountId === account?.id;
                    return (
                      <li key={owner.accountId} style={chipStyle}>
                        <span className="mono">{label}</span>
                        {isSelf && <span style={youBadgeStyle}>You</span>}
                        <button
                          type="button"
                          aria-label={`Remove ${label}`}
                          onClick={() => handleRemoveOwner(owner.accountId)}
                          disabled={ownerBusy}
                          style={removeChipButtonStyle}
                        >
                          ×
                        </button>
                      </li>
                    );
                  })}
                </ul>
                <div style={addOwnerRowStyle}>
                  <input
                    type="text"
                    aria-label="Add co-owner by callsign"
                    value={addCallsign}
                    onChange={(event) => setAddCallsign(event.target.value)}
                    disabled={ownerBusy}
                    style={controlStyle}
                  />
                  <button
                    type="button"
                    onClick={handleAddOwner}
                    disabled={ownerBusy}
                    style={addOwnerButtonStyle}
                  >
                    + Add owner
                  </button>
                </div>
                {ownerError !== null && (
                  <p role="alert" style={errorStyle}>
                    {messageForProblem(ownerError)}
                  </p>
                )}
              </fieldset>
            )}

            {creating && (
              <input
                type="text"
                name="hp_field"
                aria-label="Leave this field blank"
                tabIndex={-1}
                autoComplete="off"
                value={honeypot}
                onChange={(event) => setHoneypot(event.target.value)}
                style={{
                  position: "absolute",
                  left: "-9999px",
                  width: "1px",
                  height: "1px",
                  overflow: "hidden",
                }}
              />
            )}
          </form>

          <aside style={railStyle}>
            <div style={railCardStyle}>
              <div style={railHeadingStyle}>Visibility</div>
              <div
                role="radiogroup"
                aria-label="Visibility"
                style={{ display: "flex", flexDirection: "column", gap: "var(--space-2)" }}
              >
                {VISIBILITIES.map((v) => (
                  <VisibilityCard
                    key={v}
                    value={v}
                    checked={form.visibility === v}
                    onSelect={() => set("visibility")(v)}
                    disabled={busy}
                  />
                ))}
              </div>
            </div>

            <div style={railCardStyle}>
              <div style={railHeadingStyle}>Summary</div>
              <div role="region" aria-label="Summary" style={summaryBodyStyle}>
                {/* Read off the connection list, never off `form`: an
                    internet-only net has no frequency to summarise, and a
                    frequency shown here must be one of the ways in below. */}
                <div style={summaryRowStyle}>
                  <span style={labelStyle}>Frequency</span>
                  <span className="mono" style={summaryValueStyle}>
                    {summaryRf.frequency === null
                      ? "—"
                      : `${summaryRf.frequency} MHz`}
                  </span>
                </div>
                <div style={summaryRowStyle}>
                  <span style={labelStyle}>Band · mode</span>
                  <span style={summaryValueStyle}>
                    {summaryRf.bandMode ?? "—"}
                  </span>
                </div>
                <div style={summaryRowStyle}>
                  <span style={labelStyle}>Type</span>
                  <span style={summaryValueStyle}>
                    {sentenceCase(form.netType)}
                  </span>
                </div>
                <div style={summaryRowStyle}>
                  <span style={labelStyle}>Cadence</span>
                  <span style={summaryValueStyle}>
                    {cadenceSummary(schedule, creating)}
                  </span>
                </div>
              </div>
            </div>

            <div style={actionStackStyle}>
              <button
                type="button"
                onClick={handleSave}
                disabled={saveBlocked}
                style={primaryButtonStyle}
              >
                {creating ? "Create net" : "Save changes"}
              </button>
              {creating && (
                <p style={startSessionCaptionStyle}>
                  You&apos;ll set the schedule (date/time, recurrence) on the
                  next screen, right after creating.
                </p>
              )}
              {!creating && id !== undefined && (
                <div>
                  <p style={startSessionCaptionStyle}>Start a live session</p>
                  <StartNetControl definitionId={id} />
                </div>
              )}
              <button
                type="button"
                onClick={handleCancel}
                style={cancelButtonStyle}
              >
                Cancel
              </button>
              {state.status === "error" && connectionRowError === null && (
                <p data-testid="save-error" role="alert" style={errorStyle}>
                  {messageForProblem(state.problem)}
                </p>
              )}
            </div>
          </aside>
        </div>
      </Panel>

      {!creating && linkToken !== null && (
        <label style={{ ...fieldStyle, marginBottom: "var(--space-4)" }}>
          <span style={labelStyle}>Share link</span>
          <input
            type="text"
            readOnly
            aria-label="Share link"
            value={`${window.location.origin}${netPermalink(linkToken)}`}
            style={controlStyle}
          />
        </label>
      )}

      {!creating && (
        <section style={{ marginBottom: "var(--space-4)" }}>
          <h2 style={{ ...labelStyle, marginBottom: "var(--space-2)" }}>
            Delivery
          </h2>
          <p style={{ ...labelStyle, textTransform: "none", marginBottom: "var(--space-2)" }}>
            When a session of this net closes, its summary is delivered to these
            targets. Leave them all empty for no delivery.
          </p>
          <div style={{ marginBottom: "var(--space-3)" }}>
            <span style={labelStyle}>Delivery emails</span>
            <ul style={{ listStyle: "none", padding: 0, margin: "var(--space-1) 0 0" }}>
              {deliveryEmails.map((email, index) => (
                <li
                  key={index}
                  style={{
                    display: "flex",
                    gap: "var(--space-2)",
                    alignItems: "center",
                    marginBottom: "var(--space-1)",
                  }}
                >
                  <input
                    type="email"
                    aria-label={`Delivery email ${index + 1}`}
                    value={email}
                    onChange={(event) => setEmailAt(index)(event.target.value)}
                    disabled={deliveryBusy}
                    style={{ ...controlStyle, flex: "1 1 240px" }}
                  />
                  <button
                    type="button"
                    aria-label={`Remove delivery email ${index + 1}`}
                    onClick={() => removeEmailAt(index)}
                    disabled={deliveryBusy}
                    style={secondaryButtonStyle}
                  >
                    Remove
                  </button>
                </li>
              ))}
            </ul>
            <button
              type="button"
              onClick={addEmail}
              disabled={deliveryBusy}
              style={{ ...secondaryButtonStyle, marginTop: "var(--space-1)" }}
            >
              Add email
            </button>
          </div>
          <label style={{ ...fieldStyle, marginBottom: "var(--space-2)" }}>
            <span style={labelStyle}>Webhook URL</span>
            <input
              type="url"
              aria-label="Webhook URL"
              value={webhookUrl}
              onChange={(event) => setWebhookUrl(event.target.value)}
              disabled={deliveryBusy}
              style={controlStyle}
            />
          </label>
          <label style={{ ...fieldStyle, marginBottom: "var(--space-2)" }}>
            <span style={labelStyle}>Discord webhook URL</span>
            <input
              type="url"
              aria-label="Discord webhook URL"
              value={discordWebhookUrl}
              onChange={(event) => setDiscordWebhookUrl(event.target.value)}
              disabled={deliveryBusy}
              style={controlStyle}
            />
          </label>
          <p style={{ ...labelStyle, textTransform: "none", marginBottom: "var(--space-2)" }}>
            A Discord webhook URL is a credential — treat it like a password.
            Anyone who has it can post to that channel. If it leaks, delete the
            webhook in Discord and create a new one. NetRoll posts a short
            announcement with a link back to the session, never the roster.
          </p>
          <p style={{ ...labelStyle, textTransform: "none", marginBottom: "var(--space-2)" }}>
            A webhook URL is checked for obvious problems when you save, but that
            is not a guarantee: it is re-checked against safety rules at the
            moment each summary is actually sent, and delivery can still be
            refused then.
          </p>
          {webhookSecretSet && revealedSecret === null && (
            <p data-testid="webhook-secret-set" style={labelStyle}>
              A webhook signing secret is set for this net.
            </p>
          )}
          {revealedSecret !== null && (
            <div
              data-testid="webhook-secret-reveal"
              style={{
                border: "1px solid var(--border)",
                borderRadius: "var(--rounded-md)",
                padding: "var(--space-3)",
                marginBottom: "var(--space-2)",
              }}
            >
              <p style={{ ...labelStyle, textTransform: "none" }}>
                Copy this webhook signing secret now — it is shown only once and
                cannot be retrieved again. Use it to verify the signature on
                delivered payloads.
              </p>
              <div style={{ display: "flex", gap: "var(--space-2)" }}>
                <input
                  type="text"
                  readOnly
                  aria-label="Webhook signing secret"
                  value={revealedSecret}
                  style={{ ...controlStyle, flex: "1 1 240px" }}
                />
                <button
                  type="button"
                  aria-label="Copy webhook signing secret"
                  onClick={() => {
                    void navigator.clipboard?.writeText(revealedSecret);
                  }}
                  style={secondaryButtonStyle}
                >
                  Copy
                </button>
              </div>
            </div>
          )}
          <div style={{ display: "flex", gap: "var(--space-2)" }}>
            <button
              type="button"
              onClick={handleSaveDelivery}
              disabled={deliveryBusy}
              style={primaryButtonStyle}
            >
              Save delivery
            </button>
            <button
              type="button"
              onClick={handleClearDelivery}
              disabled={deliveryBusy}
              style={secondaryButtonStyle}
            >
              Clear delivery
            </button>
          </div>
          {deliveryError !== null && (
            <p role="alert" style={errorStyle}>
              {messageForProblem(deliveryError)}
            </p>
          )}
        </section>
      )}
    </main>
  );
}
