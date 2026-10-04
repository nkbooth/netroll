// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * How one way to reach a net reads on a surface that only DISPLAYS it — the
 * public net page and the My Nets card.
 *
 * One presenter for both, because the knowledge "which properties does a
 * `dmr` connection have, and what are they called" is the thing that would
 * otherwise be written twice and drift once.
 *
 * Unrelated to `features/session/ConnectionStatus.tsx`'s `connectionPresentation`,
 * which is about the WebSocket transport's state. A net connection is a way to
 * reach a net; the two words collide and the meanings do not.
 */

import { formatFrequencyMhz, type NetConnection } from "./netsApi";

/**
 * The connection kinds a net owner can choose from, in the order the picker
 * offers them.
 *
 * `other` is deliberately NOT here. It is instrumentation — a count of
 * `other` labels is what says whether this closed set is cut wrong — and a
 * ninth dropdown entry sitting alphabetically beside the real kinds makes that
 * instrument read high for no reason. It is reached through its own control.
 */
export const CONNECTION_KINDS = [
  "hf",
  "repeater",
  "echolink",
  "allstar",
  "dmr",
  "dstar",
  "ysf",
  "urf",
] as const;

/**
 * Every kind the domain names: the eight the picker offers plus `other`.
 *
 * A closed union rather than `string` so the maps keyed on it are TOTAL. A
 * later story that adds a kind — or a property to one — meets a compile error
 * here instead of a save that silently discards what the owner typed.
 */
export type ConnectionKind = (typeof CONNECTION_KINDS)[number] | "other";

/** The kinds reached over RF, and therefore the only ones with a frequency. */
const RF_KINDS: ReadonlySet<string> = new Set<ConnectionKind>(["hf", "repeater"]);

/** True when this kind is reached over the air, and so carries a frequency. */
export function isRfKind(kind: string): boolean {
  return RF_KINDS.has(kind);
}

/** The kinds addressed by a reflector — three naming conventions, one field. */
export const REFLECTOR_KINDS: ReadonlySet<string> = new Set<ConnectionKind>([
  "dstar",
  "ysf",
  "urf",
]);

/**
 * What each kind is CALLED, wherever one is named to a person — a picker
 * option, a card summary, a row prefix. Never the wire token: no net owner
 * knows what `urf` is.
 */
export const KIND_LABELS: Record<ConnectionKind, string> = {
  hf: "HF",
  repeater: "Repeater",
  echolink: "EchoLink",
  allstar: "AllStar",
  dmr: "DMR",
  dstar: "D-Star",
  ysf: "System Fusion",
  urf: "URF",
  other: "Other",
};

/** The reserved `label` namespace a machine-classified `other` lives in. */
const UNCLASSIFIED_COUNT_KEY = "unclassified";

/**
 * Case-folds a label and flattens every run of non-alphanumeric characters to
 * one space — the frontend twin of the domain's `count_key_of`.
 *
 * The fold is what makes the reserved namespace un-typeable: without it a
 * capital, a space for the hyphen, or a U+2011 non-breaking hyphen each mints
 * a label that reads as machine residue while being owner-authored.
 */
function countKey(label: string): string {
  return label
    .toLowerCase()
    .split(/[^\p{L}\p{N}]+/u)
    .filter((part) => part !== "")
    .join(" ");
}

/**
 * True when `label` falls in the machine-only `unclassified-` namespace — a
 * value NetRoll minted because nothing could classify it, never something an
 * owner wrote.
 *
 * Written to mirror the domain's `is_reserved_label` on the same count key,
 * but it is a HAND-WRITTEN TWIN in a second language, not a shared
 * implementation: this fold splits on `\p{L}\p{N}` where Rust's splits on
 * `char::is_alphanumeric`, and nothing makes the two agree. Treat a
 * disagreement as possible — the server is the authority, and a label this
 * says is free may still come back a 400.
 */
export function isReservedConnectionLabel(label: string): boolean {
  const key = countKey(label);
  return (
    key === UNCLASSIFIED_COUNT_KEY ||
    key.startsWith(`${UNCLASSIFIED_COUNT_KEY} `)
  );
}

/** One connection rendered for reading. */
export interface ConnectionDisplay {
  id: string;
  kind: string;
  /** What to call this way of reaching the net. */
  kindLabel: string;
  /** The facts this connection carries, in the order a visitor uses them. */
  facts: readonly (readonly [string, string])[];
  /** Whether this connection is reached over RF and so has a frequency. */
  rf: boolean;
}

/** A signed MHz offset, the way a repeater pair is written on the air. A
 * positive offset carries an explicit "+" so the direction is unambiguous. */
function offsetMhz(hz: number): string {
  return `${hz > 0 ? "+" : ""}${formatFrequencyMhz(hz)} MHz`;
}

function toneText(
  toneMode: string | null,
  toneValue: string | null,
): string | null {
  if (toneValue === null || toneValue === "") {
    return null;
  }
  return toneMode === null ? toneValue : `${toneMode.toUpperCase()} ${toneValue}`;
}

function pushIfSet(
  facts: (readonly [string, string])[],
  label: string,
  value: string | null,
): void {
  if (value !== null && value !== "") {
    facts.push([label, value] as const);
  }
}

/**
 * Describes one connection with EXACTLY the properties its kind carries.
 *
 * An `other` whose label is machine residue is described by its prose alone:
 * publishing `unclassified-reflector` to a visitor states a NetRoll internal
 * as if it were a fact about the net.
 */
export function describeConnection(connection: NetConnection): ConnectionDisplay {
  const facts: (readonly [string, string])[] = [];
  const rf = RF_KINDS.has(connection.kind);
  if (rf && connection.plannedFrequencyHz !== null) {
    const band = connection.band === null ? "" : ` · ${connection.band}`;
    const mode = connection.mode === null ? "" : ` · ${connection.mode}`;
    facts.push([
      "Frequency",
      `${formatFrequencyMhz(connection.plannedFrequencyHz)} MHz${band}${mode}`,
    ] as const);
  }
  if (connection.repeaterOffsetHz !== null) {
    facts.push(["Offset", offsetMhz(connection.repeaterOffsetHz)] as const);
  }
  pushIfSet(facts, "Tone", toneText(connection.toneMode, connection.toneValue));
  pushIfSet(facts, "Node", connection.node);
  pushIfSet(facts, "Reflector", connection.reflector);
  // Before the talkgroup: "TG 3100" is a different net on each network, so the
  // network is what makes the number readable.
  pushIfSet(facts, "Network", connection.network);
  pushIfSet(facts, "Talkgroup", connection.talkgroup);
  const authoredLabel =
    connection.kind === "other" &&
    connection.label !== null &&
    !isReservedConnectionLabel(connection.label)
      ? connection.label
      : null;
  if (connection.kind === "other") {
    pushIfSet(facts, authoredLabel ?? "Detail", connection.detail);
  }
  return {
    id: connection.id,
    kind: connection.kind,
    kindLabel:
      authoredLabel ??
      KIND_LABELS[connection.kind as ConnectionKind] ??
      connection.kind,
    facts,
    rf,
  };
}

/**
 * A check-in's `via` on the wire: a `kind`-discriminated object,
 * never two sibling nullable keys, so "both set" and "neither set" cannot be
 * represented. The hand-written twin of the Rust `ViaWire`.
 */
export type ViaWire =
  | { readonly kind: "connection"; readonly connectionId: string }
  | { readonly kind: "unlisted"; readonly text: string };

/**
 * What a check-in's `via` says when the connection it names is not in the
 * session's snapshot.
 *
 * The hand-written twin of the Rust `UNRESOLVABLE_VIA_LABEL`, in the sense
 * `isReservedConnectionLabel` is the twin of `is_reserved_label`: written to
 * mirror it, NOT shared with it, and a disagreement between the two is a defect
 * in whichever was edited alone. It NAMES the fault rather than reciting the
 * mechanism, and it is deliberately none of: blank, the UUID's
 * string form, or the ADIF-export connection's label.
 */
export const UNRESOLVABLE_VIA_LABEL = "A way in this net no longer lists";

/**
 * What ONE connection is called wherever it is named to a person: the kind's
 * name plus the fact that identifies THAT connection.
 *
 * The identifying fact is load-bearing, not decoration — a net may list two
 * repeaters, and a label of "Repeater" alone would say the same thing about two
 * different ways in. The hand-written twin of the Rust `connection_label`; the
 * two are not shared and must be edited together.
 */
export function connectionLabel(connection: NetConnection): string {
  const described = describeConnection(connection);
  const detail = identifyingFact(connection);
  return detail === null
    ? described.kindLabel
    : `${described.kindLabel} — ${detail}`;
}

/** The one fact that tells this connection apart from another of its kind. */
function identifyingFact(connection: NetConnection): string | null {
  if (RF_KINDS.has(connection.kind)) {
    return connection.plannedFrequencyHz === null
      ? null
      : `${formatFrequencyMhz(connection.plannedFrequencyHz)} MHz`;
  }
  if (connection.kind === "echolink" || connection.kind === "allstar") {
    return connection.node;
  }
  if (connection.kind === "dmr") {
    if (connection.talkgroup === null) {
      return null;
    }
    // "TG 3100" is a different net on each network, so the network is what
    // makes the number readable — the same ordering `describeConnection` uses.
    return connection.network === null
      ? `TG ${connection.talkgroup}`
      : `TG ${connection.talkgroup} on ${connection.network}`;
  }
  if (REFLECTOR_KINDS.has(connection.kind)) {
    return connection.reflector;
  }
  return connection.detail;
}

/**
 * What a check-in's `via` reads as, resolved against the session's connections.
 *
 * `null` means NOT RECORDED and nothing else: an unresolvable `via` and a
 * free-text one both return a string, so the two facts cannot collapse into
 * "absent". Resolution is BY ID against the set handed in — never by position,
 * `[0]` or `.find()` on anything but the id.
 */
export function viaLabel(
  via: ViaWire | null,
  connections: readonly NetConnection[],
): string | null {
  if (via === null) {
    return null;
  }
  if (via.kind === "unlisted") {
    return via.text;
  }
  const found = connections.find((c) => c.id === via.connectionId);
  return found === undefined ? UNRESOLVABLE_VIA_LABEL : connectionLabel(found);
}

/** Describes every connection in the owner's order. */
export function describeConnections(
  connections: readonly NetConnection[],
): ConnectionDisplay[] {
  return connections.map(describeConnection);
}

/**
 * The connection a frequency pill can describe: the FIRST RF one in the
 * owner's order, or `null` for a net reached only over the internet.
 *
 * "First in the owner's order" rather than "position 0" because a net whose
 * first connection is an EchoLink node still has a frequency worth showing; a
 * net with none has no frequency at all, and inventing one from the flat
 * mirror columns is what publishes a stale number to the world as fact.
 */
export function rfConnection(
  connections: readonly NetConnection[],
): NetConnection | null {
  return (
    connections.find(
      (c) => RF_KINDS.has(c.kind) && c.plannedFrequencyHz !== null,
    ) ?? null
  );
}

/**
 * The set summarised as ONE LINE PER THING WORTH SHOWING — the array
 * {@link connectionSummary} joins.
 *
 * It exists because a caller that must CAP the list (the discovery row's
 * 150px cell) needs the parts before the join, and the only other
 * ways to get them are a parser over the joined string — wrong the moment a
 * fact value contains the separator, which an RF fact already does — or a
 * second `describeConnections` walk, which is the drift `ConnectionPill`'s
 * docstring exists to prevent.
 *
 * **The RF branch returns exactly ONE line however many ways in the net has.**
 * That is the shipped narrower question a listing card asks ("what band is
 * this net on?"), not an oversight: widening it re-opens the deferred
 * duplicate-match-pill question and changes what three shipped tests read.
 */
export function connectionSummaryLines(
  connections: readonly NetConnection[],
): string[] {
  const rf = rfConnection(connections);
  if (rf !== null && rf.plannedFrequencyHz !== null) {
    const band = rf.band === null ? "" : ` · ${rf.band}`;
    const mode = rf.mode === null ? "" : ` · ${rf.mode}`;
    return [`${formatFrequencyMhz(rf.plannedFrequencyHz)}${band}${mode}`];
  }
  return describeConnections(connections).map((d) => {
    const first = d.facts[0];
    return first === undefined ? d.kindLabel : `${d.kindLabel} ${first[1]}`;
  });
}

/**
 * A compact one-line summary of the whole set, for a listing card: the RF
 * connection's frequency and band when there is one, else the named ways the
 * net is reached.
 *
 * Defined as the join of {@link connectionSummaryLines} and nothing else, so
 * the two cannot drift: change what a line says and this changes with it.
 */
export function connectionSummary(
  connections: readonly NetConnection[],
): string {
  return connectionSummaryLines(connections).join(" · ");
}

/**
 * Every way a LIVE SESSION is reachable, on one line — the live page's header.
 *
 * Distinct from {@link connectionSummary}, which answers a listing card's
 * narrower question ("what band is this net on?") and deliberately shows only
 * the RF way. A session header must show ALL of them: an operator watching a
 * cross-mode net needs to know the EchoLink node as much as the frequency, and
 * a session can have both.
 *
 * Built from {@link describeConnection}'s own facts rather than from a second
 * reading of the raw fields, so a kind that gains a property gains it here too.
 */
export function sessionWaysIn(connections: readonly NetConnection[]): string {
  return describeConnections(connections)
    .map((display) => {
      const first = display.facts[0];
      return first === undefined
        ? display.kindLabel
        : `${display.kindLabel} ${first[1]}`;
    })
    .join(" · ");
}
