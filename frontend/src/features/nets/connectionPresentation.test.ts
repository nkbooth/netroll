// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import {
  CONNECTION_KINDS,
  UNRESOLVABLE_VIA_LABEL,
  connectionLabel,
  connectionSummary,
  connectionSummaryLines,
  describeConnection,
  describeConnections,
  isReservedConnectionLabel,
  rfConnection,
  viaLabel,
} from "./connectionPresentation";
import type { NetConnection } from "./netsApi";

function connection(overrides: Partial<NetConnection>): NetConnection {
  return {
    id: "conn-0",
    position: 0,
    kind: "hf",
    plannedFrequencyHz: null,
    band: null,
    mode: null,
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

describe("connectionPresentation", () => {
  it("offers the eight named kinds and never `other`", () => {
    expect(CONNECTION_KINDS).toEqual([
      "hf",
      "repeater",
      "echolink",
      "allstar",
      "dmr",
      "dstar",
      "ysf",
      "urf",
    ]);
  });

  it("renders only the properties the kind carries", () => {
    const dmr = describeConnection(
      connection({ kind: "dmr", talkgroup: "3100" }),
    );
    expect(dmr.facts.map(([label]) => label)).toEqual(["Talkgroup"]);
    expect(dmr.rf).toBe(false);

    // The network qualifies the number, so it is read first: "Brandmeister,
    // talkgroup 3100" is how an operator says it.
    const dmrOnANetwork = describeConnection(
      connection({ kind: "dmr", talkgroup: "3100", network: "Brandmeister" }),
    );
    expect(dmrOnANetwork.facts).toEqual([
      ["Network", "Brandmeister"],
      ["Talkgroup", "3100"],
    ]);

    const repeater = describeConnection(
      connection({
        kind: "repeater",
        plannedFrequencyHz: 146_940_000,
        band: "2m",
        mode: "fm",
        repeaterOffsetHz: -600_000,
        toneMode: "ctcss",
        toneValue: "100.0",
      }),
    );
    expect(repeater.rf).toBe(true);
    expect(repeater.facts.map(([label]) => label)).toEqual([
      "Frequency",
      "Offset",
      "Tone",
    ]);
  });

  it("picks the first RF connection in the owner's order for the pill, or none", () => {
    const echolink = connection({ id: "a", kind: "echolink", node: "12345" });
    const hf = connection({
      id: "b",
      position: 1,
      kind: "hf",
      plannedFrequencyHz: 14_230_000,
      band: "20m",
      mode: "ssb",
    });
    expect(rfConnection([echolink, hf])?.id).toBe("b");
    expect(rfConnection([echolink])).toBeNull();
  });

  it("keeps machine residue off a public surface but never loses the value", () => {
    const residue = describeConnection(
      connection({
        kind: "other",
        label: "unclassified-reflector",
        detail: "REF030 C",
      }),
    );
    expect(residue.kindLabel).not.toContain("unclassified");
    expect(residue.facts.map(([, value]) => value)).toContain("REF030 C");
  });

  it("decides the reserved namespace on the count key, as the domain does", () => {
    expect(isReservedConnectionLabel("unclassified-reflector")).toBe(true);
    // Case, a space for the hyphen, and a non-breaking hyphen are all inside.
    expect(isReservedConnectionLabel("Unclassified Reflector")).toBe(true);
    expect(isReservedConnectionLabel("unclassified‑reflector")).toBe(true);
    expect(isReservedConnectionLabel("Wires-X")).toBe(false);
    expect(isReservedConnectionLabel("unclassifiedish")).toBe(false);
  });

  it("describes every connection in the set, in order", () => {
    const set = [
      connection({ id: "a", kind: "echolink", node: "12345" }),
      connection({ id: "b", position: 1, kind: "dmr", talkgroup: "3100" }),
    ];
    expect(describeConnections(set).map((d) => d.id)).toEqual(["a", "b"]);
  });
});

describe("the via label", () => {
  const hfA = connection({
    id: "conn-a",
    kind: "hf",
    plannedFrequencyHz: 14_230_000,
    band: "20m",
    mode: "ssb",
  });
  const hfB = connection({
    id: "conn-b",
    position: 1,
    kind: "hf",
    plannedFrequencyHz: 7_185_000,
    band: "40m",
    mode: "ssb",
  });
  const set = [hfA, hfB];

  it("tells two connections of the SAME KIND apart", () => {
    // A label of the bare kind name would say the same thing about two
    // different ways in, which is a quieter version of the failure `via` exists
    // to fix.
    expect(connectionLabel(hfA)).not.toBe(connectionLabel(hfB));
  });

  it("names the kind the way a person says it, not the wire token", () => {
    expect(
      connectionLabel(connection({ kind: "dstar", reflector: "REF030 C" })),
    ).toBe("D-Star — REF030 C");
    expect(
      connectionLabel(
        connection({ kind: "dmr", talkgroup: "3100", network: "Brandmeister" }),
      ),
    ).toBe("DMR — TG 3100 on Brandmeister");
  });

  it("never publishes a machine-minted `other` label as the connection's name", () => {
    const label = connectionLabel(
      connection({ kind: "other", label: "unclassified-reflector", detail: "XLX950 D" }),
    );
    expect(label).not.toContain("unclassified");
    expect(label).toBe("Other — XLX950 D");
  });

  it("names an owner-authored `other` by the owner's own words", () => {
    // The Rust twin pins `"Zello — channel netroll"` for this same shape
    // (`wire.rs`). Only the reserved machine namespace is suppressed; a label the
    // owner typed IS the connection's name.
    expect(
      connectionLabel(
        connection({ kind: "other", label: "Zello", detail: "channel netroll" }),
      ),
    ).toBe("Zello — channel netroll");
  });

  it("labels an RF way in with the frequency it actually carries, to the Hz", () => {
    // ⚠️ THE TWINS' SHARED LITERAL. The Rust `connection_label` asserts this
    // exact string in `net/wire.rs`
    // (`a_sub_khz_way_in_is_labelled_with_the_frequency_it_actually_carries`).
    // The Rust side once rendered `hz as f64 / 1e6` to three places, which
    // printed `145.513 MHz` here — a DIFFERENT frequency — and nothing caught it
    // because no test on either side pinned an RF label to a literal. This is
    // that pin.
    expect(
      connectionLabel(
        connection({ kind: "repeater", plannedFrequencyHz: 145_512_500 }),
      ),
    ).toBe("Repeater — 145.512.5000 MHz");
    // The whole-kHz rendering both twins already shipped is unchanged.
    expect(
      connectionLabel(connection({ kind: "hf", plannedFrequencyHz: 14_230_000 })),
    ).toBe("HF — 14.230 MHz");
  });

  it("tells two ways in one 12.5 kHz raster step apart apart", () => {
    const a = connection({ id: "a", kind: "repeater", plannedFrequencyHz: 145_512_100 });
    const b = connection({ id: "b", kind: "repeater", plannedFrequencyHz: 145_512_300 });
    expect(connectionLabel(a)).not.toBe(connectionLabel(b));
  });

  it("resolves a via BY ID, never by position", () => {
    expect(viaLabel({ kind: "connection", connectionId: "conn-b" }, set)).toBe(
      connectionLabel(hfB),
    );
  });

  it("keeps `not recorded` and `unresolvable` as different facts", () => {
    expect(viaLabel(null, set)).toBeNull();
    const rendered = viaLabel({ kind: "connection", connectionId: "gone" }, set);
    expect(rendered).toBe(UNRESOLVABLE_VIA_LABEL);
    expect(rendered).not.toBeNull();
    expect(rendered).not.toBe("");
    expect(rendered).not.toContain("gone");
    expect(rendered).not.toBe(connectionLabel(hfA));
  });

  it("prints a free-text via verbatim", () => {
    expect(viaLabel({ kind: "unlisted", text: "phone patch" }, set)).toBe(
      "phone patch",
    );
  });
});

describe("connectionSummaryLines is the one walk, and the summary is its join", () => {
  const internetOnly = [
    connection({ id: "c-echo", position: 0, kind: "echolink", node: "12345" }),
    connection({
      id: "c-dmr",
      position: 1,
      kind: "dmr",
      talkgroup: "3100",
      network: "Brandmeister",
    }),
    connection({ id: "c-dstar", position: 2, kind: "dstar", reflector: "REF030C" }),
  ];
  const withRf = [
    connection({
      id: "c-hf",
      position: 0,
      kind: "hf",
      plannedFrequencyHz: 14_230_000,
      band: "20m",
      mode: "ssb",
    }),
    ...internetOnly,
  ];

  it("gives the internet-only branch one line per connection", () => {
    expect(connectionSummaryLines(internetOnly)).toHaveLength(
      internetOnly.length,
    );
  });

  it("gives the RF branch exactly one line, however many ways in there are", () => {
    expect(connectionSummaryLines(withRf)).toHaveLength(1);
  });

  it("makes connectionSummary the join of the lines on the internet-only branch", () => {
    expect(connectionSummary(internetOnly)).toBe(
      connectionSummaryLines(internetOnly).join(" \u00b7 "),
    );
  });

  it("makes connectionSummary the join of the lines on the RF branch", () => {
    expect(connectionSummary(withRf)).toBe(
      connectionSummaryLines(withRf).join(" \u00b7 "),
    );
  });

  it("describes each connection by its own kind, so a line is not a repeat of its neighbour", () => {
    const lines = connectionSummaryLines(internetOnly);
    expect(new Set(lines).size).toBe(lines.length);
    lines.forEach((line, index) => {
      expect(line).toContain(
        describeConnections(internetOnly)[index]!.kindLabel,
      );
    });
  });

  it("returns a fresh array the caller cannot write back through", () => {
    const first = connectionSummaryLines(internetOnly);
    const second = connectionSummaryLines(internetOnly);
    expect(first).not.toBe(second);
    expect(first).toEqual(second);
  });
});
