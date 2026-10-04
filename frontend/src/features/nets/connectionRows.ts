// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * The editor's row model: one way to reach a net as the strings its inputs
 * hold, plus the conversions between that and the wire.
 *
 * Separate from `ConnectionListEditor.tsx` because these are pure functions,
 * and a component module that also exports them costs the file its fast
 * refresh.
 */

import {
  REFLECTOR_KINDS,
  isReservedConnectionLabel,
  isRfKind,
  type ConnectionKind,
} from "./connectionPresentation";
import {
  hzToMhz,
  mhzToHz,
  parseMhz,
  type MhzFault,
  type NetConnection,
  type NetConnectionInput,
} from "./netsApi";

/**
 * Every property a connection can carry a typed value in, as one closed union.
 *
 * This is the list the maps below are keyed on, and it is why a property added
 * for a later story cannot go unnoticed: adding a member here makes
 * [`TEXT_WIRE_KEYS`]/[`HZ_WIRE_KEYS`] and [`FIELD_LABELS`] incomplete, and
 * `tsc` says so.
 *
 * These are the ROW's names, not the wire's. `plannedFrequency` and
 * `repeaterOffset` name what the owner TYPES — decimal-MHz strings, labelled
 * "Frequency (MHz)" / "Offset (MHz)" — while the wire carries
 * `plannedFrequencyHz` / `repeaterOffsetHz` as integers. Only
 * [`rowToInput`] crosses between the two; renaming the union to match the wire
 * would churn every map here and the form's summary rail for no behaviour.
 */
export type ConnectionProperty = HzProperty | TextProperty;

/** The two properties typed as MHz and shipped as Hz. */
type HzProperty = "plannedFrequency" | "repeaterOffset";

/** Every property shipped as the string its input holds. */
type TextProperty =
  | "band"
  | "mode"
  | "toneMode"
  | "toneValue"
  | "node"
  | "reflector"
  | "network"
  | "talkgroup"
  | "label"
  | "detail";

/**
 * One row of the editor: every property as the string its input holds, plus
 * the identity and residue facts the row carries but never lets an owner type.
 *
 * Flat with empty strings rather than per-kind variants because a row survives
 * a kind change — an owner who picks `dmr`, then goes back to `repeater`,
 * finds what they typed. Only [`rowToInput`] decides what actually ships, and
 * it keys on `kind` alone.
 *
 * The properties come in as `Record<ConnectionProperty, string>` rather than as
 * eleven declared fields, so a new one has exactly one place to be added and
 * every map keyed on the union breaks until it is handled.
 */
export type ConnectionRow = Record<ConnectionProperty, string> & {
  /** The server-minted id, echoed so the connection keeps its identity across
   * an edit; `null` for a row the owner just added. */
  id: string | null;
  kind: ConnectionKind;
  /** True when `label` is machine residue. Such a label is echoed verbatim and
   * is never bound to an input: the domain admits it ONLY as the exact echo of
   * the one this connection already holds, so an edit would be a `400`. */
  reservedLabel: boolean;
};

/** The most connections one net may hold — the domain's `MAX_CONNECTIONS`,
 * mirrored so an owner meets the ceiling as a disabled control rather than as
 * a 400 on a list they have already finished typing. */
export const MAX_CONNECTIONS = 32;

/** The properties each kind actually carries, in the order they are asked for.
 * TOTAL over the kind union: a new kind does not compile until it is here. */
export const KIND_PROPERTIES: Record<
  ConnectionKind,
  readonly ConnectionProperty[]
> = {
  hf: ["plannedFrequency", "band", "mode"],
  repeater: [
    "plannedFrequency",
    "band",
    "mode",
    "repeaterOffset",
    "toneMode",
    "toneValue",
  ],
  echolink: ["node"],
  allstar: ["node"],
  // An operator says "Brandmeister, talkgroup 3100": the network is what makes
  // the number mean something, so it is asked for first.
  dmr: ["network", "talkgroup"],
  dstar: ["reflector"],
  ysf: ["reflector"],
  urf: ["reflector"],
  other: ["label", "detail"],
};

/** The properties a kind cannot be saved without. */
const REQUIRED_PROPERTIES: Record<
  ConnectionKind,
  readonly ConnectionProperty[]
> = {
  hf: ["plannedFrequency"],
  repeater: ["plannedFrequency"],
  echolink: ["node"],
  allstar: ["node"],
  dmr: ["talkgroup"],
  dstar: ["reflector"],
  ysf: ["reflector"],
  urf: ["reflector"],
  other: ["label"],
};

/** True when `kind` is one the editor knows how to render. */
function isConnectionKind(kind: string): kind is ConnectionKind {
  return Object.hasOwn(KIND_PROPERTIES, kind);
}

/** The properties `kind` carries, or none for a kind this build never heard
 * of — a server that grows a ninth kind renders an empty row, not a crash. */
export function propertiesOf(kind: string): readonly ConnectionProperty[] {
  return isConnectionKind(kind) ? KIND_PROPERTIES[kind] : [];
}

/** The wire keys that carry an integer. */
type HzWireKey = "plannedFrequencyHz" | "repeaterOffsetHz";

/** The wire keys that carry a string, `id` and `kind` aside. */
type TextWireKey = Exclude<keyof NetConnectionInput, "id" | "kind" | HzWireKey>;

/**
 * The wire key each row property is sent under, split by what the wire
 * carries there. Together the two maps are TOTAL over the property union — a
 * property with no wire key is a value the owner types and the save silently
 * drops, which is the failure this shape refuses — and the split is what lets
 * `tsc` see that a string goes under a string key and an integer under an
 * integer one.
 */
const HZ_WIRE_KEYS: Record<HzProperty, HzWireKey> = {
  plannedFrequency: "plannedFrequencyHz",
  repeaterOffset: "repeaterOffsetHz",
};

const TEXT_WIRE_KEYS: Record<TextProperty, TextWireKey> = {
  band: "band",
  mode: "mode",
  toneMode: "toneMode",
  toneValue: "toneValue",
  node: "node",
  reflector: "reflector",
  network: "network",
  talkgroup: "talkgroup",
  label: "label",
  detail: "detail",
};

function isHzProperty(property: ConnectionProperty): property is HzProperty {
  return Object.hasOwn(HZ_WIRE_KEYS, property);
}

/** What each property is called on its input. TOTAL over the property union. */
export const FIELD_LABELS: Record<ConnectionProperty, string> = {
  plannedFrequency: "Frequency (MHz)",
  band: "Band",
  mode: "Mode",
  repeaterOffset: "Offset (MHz)",
  toneMode: "Tone mode",
  toneValue: "Tone value",
  node: "Node",
  reflector: "Reflector",
  network: "Network",
  talkgroup: "Talkgroup",
  label: "Label",
  detail: "Detail",
};

const EMPTY_ROW: ConnectionRow = {
  id: null,
  kind: "hf",
  plannedFrequency: "",
  band: "20m",
  mode: "ssb",
  repeaterOffset: "",
  toneMode: "",
  toneValue: "",
  node: "",
  reflector: "",
  network: "",
  talkgroup: "",
  label: "",
  detail: "",
  reservedLabel: false,
};

/** A blank row of `kind`, for the add affordances. It carries NO id, so the
 * server mints one — copying an existing id would be a `DuplicateId` 400. */
export function newConnectionRow(kind: ConnectionKind): ConnectionRow {
  return { ...EMPTY_ROW, kind };
}

/**
 * Seeds a row from a connection as the API SERVES it.
 *
 * Frequencies come in through `hzToMhz`, whose minimal output round-trips
 * back to the same integer — never through `formatFrequencyMhz`, whose second
 * dot (`448.670.1250`) is unparseable by design and would 400 on save.
 */
export function connectionToRow(connection: NetConnection): ConnectionRow {
  const label = connection.label ?? "";
  return {
    ...EMPTY_ROW,
    id: connection.id,
    // Cast, not a narrowing fallback: a kind this build does not know renders
    // no fields (`propertiesOf` returns none) but is still echoed VERBATIM on
    // the next save. Rewriting it to `other` would destroy the connection.
    kind: connection.kind as ConnectionKind,
    plannedFrequency:
      connection.plannedFrequencyHz === null
        ? ""
        : hzToMhz(connection.plannedFrequencyHz),
    band: connection.band ?? EMPTY_ROW.band,
    mode: connection.mode ?? EMPTY_ROW.mode,
    repeaterOffset:
      connection.repeaterOffsetHz === null
        ? ""
        : hzToMhz(connection.repeaterOffsetHz),
    toneMode: connection.toneMode ?? "",
    toneValue: connection.toneValue ?? "",
    node: connection.node ?? "",
    reflector: connection.reflector ?? "",
    network: connection.network ?? "",
    talkgroup: connection.talkgroup ?? "",
    label,
    detail: connection.detail ?? "",
    reservedLabel: isReservedConnectionLabel(label),
  };
}

/**
 * Turns a row into the entry it contributes to a `PUT .../connections` body.
 *
 * A property belonging to another kind is ABSENT, not empty: the domain reads
 * only the properties the named kind has, so a stale key would be discarded
 * with a `200` and the owner would never learn the value they typed was lost.
 * An optional property left blank is dropped for the same reason an absent one
 * is — an empty string is a value, and the server would have to refuse it.
 *
 * The two frequency properties go out as exact-Hz integers through `mhzToHz`
 * PRECONDITION: the row has passed [`firstRowRefusal`], which
 * refuses a non-empty frequency string `mhzToHz` cannot convert — `handleSave`
 * runs the gate first. If that is ever violated, the key is OMITTED rather than
 * sent as `null`, `NaN`, `Infinity` or `0`, so no wrong frequency is stored.
 * But the two properties are NOT equally protected by that omission:
 * `plannedFrequencyHz` is required on `hf`/`repeater`, so the server's
 * missing-property refusal backstops it; `repeaterOffsetHz` is optional
 * server-side, so an omitted offset is a legal absent one and the save is a
 * silent `200` with the owner's value gone. For the offset, the gate is the
 * only defence — which is why it must run first, and why `mhzToHz` returns
 * `null` for everything the wire cannot carry rather than leaving range to the
 * server.
 */
export function rowToInput(row: ConnectionRow): NetConnectionInput {
  const input: NetConnectionInput = { kind: row.kind };
  if (row.id !== null) {
    input.id = row.id;
  }
  for (const property of propertiesOf(row.kind)) {
    const value = row[property];
    if (value === "") {
      continue;
    }
    if (isHzProperty(property)) {
      const hz = mhzToHz(value);
      if (hz !== null) {
        input[HZ_WIRE_KEYS[property]] = hz;
      }
    } else {
      input[TEXT_WIRE_KEYS[property]] = value;
    }
  }
  return input;
}

/** A property's label as it reads in a sentence: lower-case, unit dropped. */
function spokenLabel(property: ConnectionProperty): string {
  return FIELD_LABELS[property].toLowerCase().replace(" (mhz)", "");
}

/** What to tell the owner about a MHz string that cannot become Hz. Each
 * fault gets its own diagnosis: "enter a decimal number" is wrong advice for
 * `14.2301234`, which is one — its fault is sub-Hz precision. */
function hzFaultMessage(property: ConnectionProperty, fault: MhzFault): string {
  const label = spokenLabel(property);
  switch (fault) {
    case "not-a-number":
      return `Enter the ${label} as a decimal number of MHz, like 146.940.`;
    case "too-precise":
      return `The ${label} has more than six decimal places. NetRoll stores whole Hz, so 0.000001 MHz is the finest step.`;
    case "too-large":
      return `The ${label} is too large to be a frequency in MHz.`;
  }
}

function rowRefusal(row: ConnectionRow): Omit<RowError, "index"> | null {
  const required = isConnectionKind(row.kind)
    ? REQUIRED_PROPERTIES[row.kind]
    : [];
  for (const property of required) {
    if (row[property] === "") {
      return {
        property,
        reason: "missing",
        message: `Add the ${spokenLabel(property)} for this connection.`,
      };
    }
  }
  // The client-side half of the faults that used to be the server's: the wire
  // carries Hz integers now, so a MHz string that cannot become one has to be
  // refused here, against the row. Nothing downstream catches a bypass for
  // both properties: `rowToInput` omits an unconvertible key, which the server
  // refuses as missing for the REQUIRED frequency but accepts as a legal
  // absent value for the OPTIONAL offset — a silent `200` with the owner's
  // offset gone. This gate is the only thing standing between the two.
  for (const property of propertiesOf(row.kind)) {
    if (!isHzProperty(property) || row[property] === "") {
      continue;
    }
    const { fault } = parseMhz(row[property]);
    if (fault !== null) {
      return {
        property,
        reason: fault,
        message: hzFaultMessage(property, fault),
      };
    }
  }
  if (
    row.kind === "other" &&
    !row.reservedLabel &&
    isReservedConnectionLabel(row.label)
  ) {
    return {
      property: "label",
      reason: "reserved-label",
      message:
        "That name is reserved for connections NetRoll could not classify — choose another.",
    };
  }
  return null;
}

/**
 * The first row a save must refuse, with its index, or `null`.
 *
 * Runs before anything is sent, so a row an owner has not finished is refused
 * against the row rather than by the server against an index — and, on
 * create, before a request that would otherwise be refused wholesale.
 */
export function firstRowRefusal(rows: readonly ConnectionRow[]): RowError | null {
  for (const [index, row] of rows.entries()) {
    const refusal = rowRefusal(row);
    if (refusal !== null) {
      return { index, ...refusal };
    }
  }
  return null;
}

/**
 * Why the pre-write gate refused a row. `missing` is a required property left
 * blank; the three [`MhzFault`]s are a frequency string that cannot become
 * Hz; `reserved-label` is an owner-typed name in the machine-only namespace.
 */
export type RowRefusalReason = "missing" | MhzFault | "reserved-label";

/**
 * A refusal attributed to one row — by the pre-write gate, which knows which
 * property is at fault and why, or by the server, which names only the entry.
 *
 * `reason` is the machine-readable diagnosis and is what a test pins; the
 * `message` is copy. It is present exactly when the gate raised the refusal —
 * the server's problem carries no equivalent, so a server-raised `RowError`
 * has none.
 *
 * Indexed by POSITION, which is all the server can say. It is therefore only
 * valid for the list it was raised against: the page clears it on any change
 * to the list, because after a reorder the same index is a different
 * connection and the message would libel it.
 */
export interface RowError {
  index: number;
  property?: ConnectionProperty;
  reason?: RowRefusalReason;
  message: string;
}

/**
 * Applies a kind change, carrying an `other`'s prose into the reflector field
 * when the new kind is one of the three reflector networks.
 *
 * That carry is the repair path for machine residue: `unclassified-reflector`
 * with `REF030 C` in its prose is a reflector whose network nothing named, and
 * this is the only place an owner can say which network it is.
 */
export function withKind(row: ConnectionRow, kind: ConnectionKind): ConnectionRow {
  if (row.kind === kind) {
    return row;
  }
  const carried =
    row.kind === "other" && REFLECTOR_KINDS.has(kind) && row.reflector === ""
      ? row.detail
      : row.reflector;
  return { ...row, kind, reflector: carried };
}

/**
 * What the Summary rail can honestly say about the list: the first RF row that
 * actually carries a frequency, or `null` for a net reached only over the
 * internet.
 *
 * `null` is the whole point: a net with no RF way has no frequency, and the
 * rail must not invent one. The list is the only place the form holds a
 * frequency at all.
 */
export function rfRow(rows: readonly ConnectionRow[]): ConnectionRow | null {
  return (
    rows.find((row) => isRfKind(row.kind) && row.plannedFrequency !== "") ?? null
  );
}
