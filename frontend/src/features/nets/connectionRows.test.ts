// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import {
  KIND_PROPERTIES,
  connectionToRow,
  firstRowRefusal,
  newConnectionRow,
  rfRow,
  rowToInput,
  type ConnectionProperty,
} from "./connectionRows";
import { CONNECTION_KINDS } from "./connectionPresentation";
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

describe("connection row conversion", () => {
  it("seeds frequency inputs from the parseable helper, not the display one", () => {
    // `formatFrequencyMhz` would give "448.670.1250", which `mhzToHz` refuses
    // and the server cannot parse.
    const row = connectionToRow(
      connection({
        kind: "hf",
        plannedFrequencyHz: 448_670_125,
        band: "70cm",
        mode: "fm",
      }),
    );
    expect(row.plannedFrequency).toBe("448.670125");
    // The write speaks the read's vocabulary — the integer the
    // server served goes back as the same integer, under the same key.
    const input = rowToInput(row);
    expect(input.plannedFrequencyHz).toBe(448_670_125);
    expect(input).not.toHaveProperty("plannedFrequency");
  });

  it("round-trips a signed repeater offset through the row", () => {
    const row = connectionToRow(
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
    expect(row.repeaterOffset).toBe("-0.6");
    // The sub-MHz negative offset is the value most likely to lose its sign.
    const input = rowToInput(row);
    expect(input.repeaterOffsetHz).toBe(-600_000);
    expect(input.plannedFrequencyHz).toBe(146_940_000);
    expect(input).not.toHaveProperty("repeaterOffset");
  });

  it("emits no key for a property the kind does not carry", () => {
    const row = { ...connectionToRow(connection({ kind: "echolink", node: "12345" })) };
    expect(Object.keys(rowToInput(row)).sort()).toEqual(["id", "kind", "node"]);
  });

  it("omits the id of a row the owner just added, so the server mints one", () => {
    const input = rowToInput(newConnectionRow("hf"));
    expect(input.id).toBeUndefined();
    expect(input.kind).toBe("hf");
  });

  it("drops an optional property left empty rather than sending an empty string", () => {
    const row = connectionToRow(
      connection({
        kind: "repeater",
        plannedFrequencyHz: 146_940_000,
        band: "2m",
        mode: "fm",
      }),
    );
    expect(Object.keys(rowToInput(row)).sort()).toEqual([
      "band",
      "id",
      "kind",
      "mode",
      "plannedFrequencyHz",
    ]);
  });
});

describe("the pre-write gate on frequency inputs", () => {
  // The write body carries Hz integers, so a MHz string the
  // client cannot convert must stop HERE, against the row and its property —
  // the server never sees text for these two fields any more.
  // The last two are numbers, but not ones that fit: a 13-digit MHz value is
  // past i64::MAX, and a ~310-digit one makes `Number` return Infinity. Both
  // must stop here too, or the first comes back from the server with no row
  // index and the second is serialized as `null` and reported as missing.
  const unparseable = [
    "14,230",
    "14.2.3",
    "abc",
    "14.2301234",
    "10000000000000",
    `1${"0".repeat(310)}`,
  ];

  it.each(unparseable)(
    "refuses a frequency it cannot convert (%s) against the row, before send",
    (typed) => {
      const rows = [
        { ...newConnectionRow("echolink"), node: "12345" },
        { ...newConnectionRow("hf"), plannedFrequency: typed },
      ];
      const refusal = firstRowRefusal(rows);
      expect(refusal).not.toBeNull();
      expect(refusal?.index).toBe(1);
      expect(refusal?.property).toBe("plannedFrequency");
    },
  );

  it.each(unparseable)(
    "refuses an offset it cannot convert (%s) on a repeater whose frequency is fine",
    (typed) => {
      const rows = [
        {
          ...newConnectionRow("repeater"),
          plannedFrequency: "146.94",
          repeaterOffset: typed,
        },
      ];
      const refusal = firstRowRefusal(rows);
      expect(refusal).not.toBeNull();
      expect(refusal?.index).toBe(0);
      expect(refusal?.property).toBe("repeaterOffset");
    },
  );

  // The server used to tell these apart (`NotNumeric`, `TooPrecise`,
  // `OutOfRange`); the faults moved to the client, and the diagnosis must
  // move with them or the owner is told to retype a value that IS a decimal
  // number of MHz. The reason is the pin; the sentence is not.
  it.each([
    ["abc", "not-a-number"],
    ["14,230", "not-a-number"],
    ["14.2.3", "not-a-number"],
    ["14.2301234", "too-precise"],
    ["10000000000000", "too-large"],
    [`1${"0".repeat(310)}`, "too-large"],
  ] as const)(
    "names WHY it refuses %s — %s — so the owner is told what to change",
    (typed, reason) => {
      const refusal = firstRowRefusal([
        { ...newConnectionRow("hf"), plannedFrequency: typed },
      ]);
      expect(refusal?.reason).toBe(reason);
    },
  );

  it("gives each frequency fault its own message, not one sentence for all three", () => {
    const messages = ["abc", "14.2301234", "10000000000000"].map(
      (typed) =>
        firstRowRefusal([{ ...newConnectionRow("hf"), plannedFrequency: typed }])
          ?.message,
    );
    expect(new Set(messages).size).toBe(3);
  });

  it("names a required property left blank as missing, not as unconvertible", () => {
    const refusal = firstRowRefusal([newConnectionRow("hf")]);
    expect(refusal?.reason).toBe("missing");
    expect(refusal?.property).toBe("plannedFrequency");
  });

  it("names an owner-typed reserved label as such", () => {
    const refusal = firstRowRefusal([
      { ...newConnectionRow("other"), label: "unclassified-reflector" },
    ]);
    expect(refusal?.reason).toBe("reserved-label");
  });

  it("still treats a blank optional offset as simply absent, never a refusal", () => {
    const rows = [{ ...newConnectionRow("repeater"), plannedFrequency: "146.94" }];
    expect(firstRowRefusal(rows)).toBeNull();
    expect(rowToInput(rows[0])).not.toHaveProperty("repeaterOffsetHz");
  });

  it("omits the Hz key rather than sending null or NaN if the gate were bypassed", () => {
    // Defence in depth: `rowToInput` is only called on rows that passed the
    // gate, but if that precondition is ever violated the key is absent — not
    // a `0`, a `NaN`, or an `Infinity` that `JSON.stringify` would write as
    // `null`. For the REQUIRED frequency the server's missing-property refusal
    // then catches it.
    for (const typed of ["abc", `1${"0".repeat(310)}`]) {
      const row = { ...newConnectionRow("hf"), plannedFrequency: typed };
      expect(rowToInput(row)).not.toHaveProperty("plannedFrequencyHz");
      expect(rowToInput(row)).not.toHaveProperty("plannedFrequency");
    }
  });

  it("omits an unconvertible OPTIONAL offset too, where no server backstop exists", () => {
    // The asymmetry the gate exists for: `repeaterOffsetHz` is optional
    // server-side, so an omitted offset is a legal absent one and the save is
    // a silent 200 with the owner's value gone. Only the gate prevents that —
    // this pins what `rowToInput` does on its own, so the reliance is visible.
    const row = {
      ...newConnectionRow("repeater"),
      plannedFrequency: "146.94",
      repeaterOffset: "abc",
    };
    expect(rowToInput(row)).not.toHaveProperty("repeaterOffsetHz");
    expect(rowToInput(row)).not.toHaveProperty("repeaterOffset");
    expect(rowToInput(row).plannedFrequencyHz).toBe(146_940_000);
  });
});

describe("the kind/property maps are total", () => {
  // The compile-time half of this is `tsc`: `KIND_PROPERTIES`, `WIRE_KEYS` and
  // `FIELD_LABELS` are keyed on closed unions, so a kind or a property added
  // for a later story cannot be left out of them. This is the runtime half —
  // it catches the case those types cannot see, a property DECLARED for a kind
  // that `rowToInput` then does not ship.
  it("ships every property every kind declares", () => {
    // The two frequency properties are typed as MHz and shipped as Hz under a
    // different key, so their fixture values must be parseable
    // and the expected key set is the wire's, not the row's.
    const parseable: Partial<Record<ConnectionProperty, string>> = {
      plannedFrequency: "14.230",
      repeaterOffset: "-0.6",
    };
    const wireKey: Partial<Record<ConnectionProperty, string>> = {
      plannedFrequency: "plannedFrequencyHz",
      repeaterOffset: "repeaterOffsetHz",
    };
    for (const kind of [...CONNECTION_KINDS, "other"] as const) {
      const row = { ...newConnectionRow(kind) };
      for (const property of KIND_PROPERTIES[kind]) {
        row[property] = parseable[property] ?? `v-${property}`;
      }
      const keys = Object.keys(rowToInput(row)).filter((k) => k !== "kind");
      const expected = KIND_PROPERTIES[kind].map((p) => wireKey[p] ?? p);
      expect(keys.sort()).toEqual([...expected].sort());
    }
  });

  it("declares a property list for every kind the picker offers", () => {
    for (const kind of CONNECTION_KINDS) {
      expect(KIND_PROPERTIES[kind].length).toBeGreaterThan(0);
    }
  });

  it("ships a DMR row's network alongside its talkgroup", () => {
    const row = {
      ...newConnectionRow("dmr"),
      id: "conn-dmr",
      network: "Brandmeister",
      talkgroup: "3100",
    };
    expect(Object.keys(rowToInput(row)).sort()).toEqual([
      "id",
      "kind",
      "network",
      "talkgroup",
    ]);
    expect(rowToInput(row).network).toBe("Brandmeister");
  });

  it("ships NO network key when the owner left the network blank", () => {
    // A blank optional property is ABSENT, never `""`: the server would have to
    // refuse an empty string, and a DMR net whose network nobody recorded must
    // stay saveable.
    const row = { ...newConnectionRow("dmr"), talkgroup: "3100" };
    expect(Object.keys(rowToInput(row)).sort()).toEqual(["kind", "talkgroup"]);
  });

  it("seeds the network input from a served DMR connection", () => {
    const row = connectionToRow(
      connection({ kind: "dmr", talkgroup: "3100", network: "TGIF" }),
    );
    expect(row.network).toBe("TGIF");
  });
});

describe("the summary's RF row", () => {
  it("is the first RF row that actually carries a frequency, not position zero", () => {
    const rows = [
      { ...newConnectionRow("echolink"), node: "12345" },
      { ...newConnectionRow("repeater"), plannedFrequency: "146.94" },
    ];
    expect(rfRow(rows)?.plannedFrequency).toBe("146.94");
  });

  it("is null for a net reached only over the internet, so nothing is invented", () => {
    // The row model seeds `band`/`mode` defaults on every row; returning an
    // internet-only row here is how a fabricated "20m · ssb" would get out.
    expect(rfRow([{ ...newConnectionRow("echolink"), node: "12345" }])).toBeNull();
    // An RF row whose frequency has not been typed yet is not one either.
    expect(rfRow([newConnectionRow("hf")])).toBeNull();
  });
});
