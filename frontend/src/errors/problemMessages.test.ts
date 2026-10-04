// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import type { Problem } from "../features/auth/authApi";
import {
  GENERIC_PROBLEM_MESSAGE,
  KNOWN_PROBLEM_TYPES,
  messageForProblem,
  messageForProblemType,
} from "./problemMessages";

// The slug list is READ OFF THE MAP, never re-typed. The hand-maintained copy
// this replaced held 30 slugs against a map of 50, so the twenty newest — the
// ones most likely to have their copy deleted by a careless edit — were outside
// every structural guard below. No number is written here on purpose: the list
// IS the map, so it cannot go stale again.
const KNOWN_TYPES = KNOWN_PROBLEM_TYPES;

const problem = (type: string | undefined): Problem | undefined =>
  type === undefined ? undefined : { type, status: 400 };

describe("problemMessages mapping", () => {
  it("maps each known problem type to its own distinct message", () => {
    const messages = KNOWN_TYPES.map(messageForProblemType);

    expect(new Set(messages).size).toBe(KNOWN_TYPES.length);
    for (const message of messages) {
      expect(message).not.toBe(GENERIC_PROBLEM_MESSAGE);
    }
  });

  it("falls back to the generic message for unknown or missing types", () => {
    expect(messageForProblemType("/errors/not-a-thing")).toBe(
      GENERIC_PROBLEM_MESSAGE,
    );
    expect(messageForProblemType(undefined)).toBe(GENERIC_PROBLEM_MESSAGE);
  });
});

describe("messageForProblem preference order", () => {
  // Every assertion here is a data-flow property — "which source won" — never a
  // piece of copy. The fixture detail is owned by this test; the counter-half is
  // `messageForProblemType(slug)`, computed, never typed out.
  const DETAIL = "the field-naming answer the server sent";

  it("prefers a problem's detail over the slug map", () => {
    const result = messageForProblem({
      type: "/errors/validation",
      status: 400,
      detail: DETAIL,
    });

    expect(result).toBe(DETAIL);
    expect(result).not.toBe(messageForProblemType("/errors/validation"));
  });

  it("falls back to the slug map when the problem carries no detail", () => {
    expect(messageForProblem(problem("/errors/validation"))).toBe(
      messageForProblemType("/errors/validation"),
    );
  });

  it("treats an empty-string detail as absent rather than rendering nothing", () => {
    // An empty `role="alert"` announces a live-region update with nothing in
    // it. Asserted as equality with the MAP, not as equality with the fixture —
    // the latter would assert the defect and go green on a bare `??`.
    const result = messageForProblem({
      type: "/errors/validation",
      status: 400,
      detail: "",
    });

    expect(result).toBe(messageForProblemType("/errors/validation"));
  });

  it("resolves an absent problem — a network failure — to the generic fallback", () => {
    expect(messageForProblem(undefined)).toBe(GENERIC_PROBLEM_MESSAGE);
  });

  it("prefers the detail of an unmapped slug over the generic fallback", () => {
    // `/errors/avatar-invalid` has no map entry, so before the resolver every
    // avatar rejection rendered the generic sentence.
    const result = messageForProblem({
      type: "/errors/avatar-invalid",
      status: 400,
      detail: DETAIL,
    });

    expect(result).toBe(DETAIL);
    expect(result).not.toBe(GENERIC_PROBLEM_MESSAGE);
  });

  it("keeps the map's copy for /errors/staying-invalid, whose detail is a token echo", () => {
    const result = messageForProblem({
      type: "/errors/staying-invalid",
      status: 400,
      detail: DETAIL,
    });

    expect(result).toBe(messageForProblemType("/errors/staying-invalid"));
    expect(result).not.toBe(DETAIL);
  });

  it("keeps the map's copy for /errors/precedence-invalid, whose detail is a token echo", () => {
    const result = messageForProblem({
      type: "/errors/precedence-invalid",
      status: 400,
      detail: DETAIL,
    });

    expect(result).toBe(messageForProblemType("/errors/precedence-invalid"));
    expect(result).not.toBe(DETAIL);
  });

  it("composes /errors/lock-held's bare callsign into a sentence rather than showing it raw", () => {
    const holder = "K4ABC";

    const result = messageForProblem({
      type: "/errors/lock-held",
      status: 409,
      detail: holder,
    });

    expect(result).toContain(holder);
    expect(result).not.toBe(holder);
    expect(result).not.toBe(messageForProblemType("/errors/lock-held"));
  });

  it("degrades /errors/lock-held to the map when no holder callsign arrived", () => {
    expect(messageForProblem(problem("/errors/lock-held"))).toBe(
      messageForProblemType("/errors/lock-held"),
    );
  });

  it("never tells a reader of an unreplayable log to try again", () => {
    // The only failure in this map a reader can do
    // NOTHING about. `GENERIC_PROBLEM_MESSAGE` is an instruction to repeat an
    // action that will never work, so it is the ONE answer this slug must never
    // resolve to. Asserted through `messageForProblem` (the resolver every
    // surface calls), not through the map half, because a `detail`-less refusal
    // is the case the map entry exists for.
    const result = messageForProblem(problem("/errors/unreplayable-log"));

    expect(result).not.toBe(GENERIC_PROBLEM_MESSAGE);
    expect(result).toBe(messageForProblemType("/errors/unreplayable-log"));
  });

  it("prefers the server's fault-naming detail for an unreplayable log", () => {
    // The backend always populates `detail` on this slug (voice),
    // and ordering means that is what the reader sees. The map
    // entry above is the belt-and-braces, not the primary answer.
    const result = messageForProblem({
      type: "/errors/unreplayable-log",
      status: 410,
      detail: DETAIL,
    });

    expect(result).toBe(DETAIL);
  });

  it("degrades /errors/lock-held to the map on an empty holder callsign", () => {
    // The composition must never emit a sentence with a hole where the
    // callsign should be.
    const result = messageForProblem({
      type: "/errors/lock-held",
      status: 409,
      detail: "",
    });

    expect(result).toBe(messageForProblemType("/errors/lock-held"));
  });
});
