// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  absoluteLocalTime,
  absoluteLocalTimeWithWeekday,
  formatDurationHm,
  formatElapsedHms,
  humanizeTime,
} from "./humanizeTime";

const at = (iso: string): Date => new Date(iso);

describe("humanizeTime", () => {
  const now = at("2026-07-16T12:00:00Z");

  it("reads sub-minute gaps in either direction as 'just now'", () => {
    // Branch: |diff| < 1 minute → "just now" regardless of sign.
    expect(humanizeTime("2026-07-16T12:00:30Z", now)).toBe("just now");
    expect(humanizeTime("2026-07-16T11:59:31Z", now)).toBe("just now");
    expect(humanizeTime("2026-07-16T12:00:00Z", now)).toBe("just now");
  });

  it("selects the minute unit under an hour, signed by direction", () => {
    expect(humanizeTime("2026-07-16T12:40:00Z", now)).toBe("in 40m");
    expect(humanizeTime("2026-07-16T11:20:00Z", now)).toBe("40m ago");
    // Boundary: exactly one minute crosses out of "just now".
    expect(humanizeTime("2026-07-16T12:01:00Z", now)).toBe("in 1m");
  });

  it("selects the hour unit under a day, signed by direction", () => {
    expect(humanizeTime("2026-07-16T14:00:00Z", now)).toBe("in 2h");
    expect(humanizeTime("2026-07-16T09:00:00Z", now)).toBe("3h ago");
    // Boundary: exactly one hour crosses out of the minute unit.
    expect(humanizeTime("2026-07-16T13:00:00Z", now)).toBe("in 1h");
  });

  it("selects the day unit at and beyond 24h, signed by direction", () => {
    expect(humanizeTime("2026-07-18T12:00:00Z", now)).toBe("in 2d");
    expect(humanizeTime("2026-07-13T12:00:00Z", now)).toBe("3d ago");
    // Boundary: exactly one day crosses out of the hour unit.
    expect(humanizeTime("2026-07-17T12:00:00Z", now)).toBe("in 1d");
  });
});

describe("absoluteLocalTime", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("routes through the platform locale formatter (not a hardcoded string)", () => {
    // Assert the formatting PATH is taken — the exact locale output is the
    // platform's, never asserted as a literal [memory: tdd-test-logic-not-text].
    const spy = vi
      .spyOn(Date.prototype, "toLocaleString")
      .mockReturnValue("LOCAL_SENTINEL");
    expect(absoluteLocalTime("2027-01-01T20:00:00Z")).toBe("LOCAL_SENTINEL");
    expect(spy).toHaveBeenCalled();
  });
});

describe("absoluteLocalTimeWithWeekday", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("names the weekday explicitly, which the terse absolute form omits", () => {
    // A wrong occurrence must be legible as wrong, and the
    // WEEKDAY is the thing that was wrong on the live instance. Assert the
    // formatting path and the options it requests — never a literal locale
    // string [memory: tdd-test-logic-not-text].
    const spy = vi
      .spyOn(Date.prototype, "toLocaleString")
      .mockReturnValue("WEEKDAY_SENTINEL");

    expect(absoluteLocalTimeWithWeekday("2026-08-26T01:30:00Z")).toBe(
      "WEEKDAY_SENTINEL",
    );
    const options = spy.mock.calls[0]?.[1] as
      | Intl.DateTimeFormatOptions
      | undefined;
    expect(options?.weekday).toBeDefined();
    expect(options?.month).toBeDefined();
    expect(options?.day).toBeDefined();
    expect(options?.hour).toBeDefined();
    expect(options?.minute).toBeDefined();
  });

  it("asks for no options at all in the terse sibling, so its caller is unaffected", () => {
    // `absoluteLocalTime` feeds RosterEntry, which wants the terse form — the
    // new helper is a SIBLING, not an edit to it.
    const spy = vi
      .spyOn(Date.prototype, "toLocaleString")
      .mockReturnValue("TERSE_SENTINEL");
    absoluteLocalTime("2026-08-26T01:30:00Z");
    expect(spy.mock.calls[0]?.[1]).toBeUndefined();
  });
});

describe("formatElapsedHms", () => {
  it("renders HH:MM:SS since the started instant (session-header Elapsed stat)", () => {
    expect(formatElapsedHms("2026-07-16T00:00:00Z", at("2026-07-16T00:41:12Z"))).toBe("00:41:12");
  });

  it("pads every segment to two digits", () => {
    expect(formatElapsedHms("2026-07-16T00:00:00Z", at("2026-07-16T01:02:03Z"))).toBe("01:02:03");
  });

  it("clamps to zero for a not-yet-started or clock-skewed instant", () => {
    expect(formatElapsedHms("2026-07-16T00:10:00Z", at("2026-07-16T00:00:00Z"))).toBe("00:00:00");
  });
});

describe("formatDurationHm", () => {
  it("renders HH:MM from a whole-seconds duration (post-net summary's Duration stat)", () => {
    expect(formatDurationHm(91 * 60)).toBe("01:31");
  });

  it("pads both segments to two digits", () => {
    expect(formatDurationHm(5 * 60 + 3)).toBe("00:05");
  });
});
