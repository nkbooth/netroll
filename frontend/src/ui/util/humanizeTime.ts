// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Humanized time display: render an absolute UTC instant as a
 * relative, local-feeling phrase ("in 2h", "40m ago", "just now") rather than
 * a raw UTC string. Pure and `now`-injected so it is deterministically
 * testable — no `Date.now()` inside (the caller passes the reference instant).
 *
 * A feature-agnostic `ui/` utility: it knows about time, not nets.
 * The discovery hero, the upcoming list, and the net form's occurrence render
 * all route through it (the three-strike DRY extraction of the inline
 * occurrence-time logic).
 */

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

/** `in <n><unit>` for a future gap, `<n><unit> ago` for a past one. */
function signed(magnitude: number, unit: string, future: boolean): string {
  return future ? `in ${magnitude}${unit}` : `${magnitude}${unit} ago`;
}

/**
 * A relative phrase for `utcIso` as seen from `now`. Unit selected by the
 * gap's magnitude: sub-minute → "just now"; under an hour → minutes; under a
 * day → hours; otherwise days. The sign (future/past) chooses "in …" vs
 * "… ago".
 */
export function humanizeTime(utcIso: string, now: Date): string {
  const diffMs = new Date(utcIso).getTime() - now.getTime();
  const future = diffMs > 0;
  const abs = Math.abs(diffMs);

  if (abs < MINUTE_MS) {
    return "just now";
  }
  if (abs < HOUR_MS) {
    return signed(Math.floor(abs / MINUTE_MS), "m", future);
  }
  if (abs < DAY_MS) {
    return signed(Math.floor(abs / HOUR_MS), "h", future);
  }
  return signed(Math.floor(abs / DAY_MS), "d", future);
}

/**
 * The absolute instant rendered in the viewer's local time — for a title /
 * tooltip that complements the relative phrase. Routes through the platform
 * locale formatter (`toLocaleString`); the exact string is the platform's.
 */
export function absoluteLocalTime(utcIso: string): string {
  return new Date(utcIso).toLocaleString();
}

/**
 * The absolute instant in the viewer's local time WITH the weekday name — the
 * companion to the relative phrase on the discovery upcoming list. A sibling
 * of [`absoluteLocalTime`] rather than an edit to it:
 * that one's caller wants the terse form, and this one exists precisely because
 * the terse form omits the weekday, which is the field a mis-stored recurrence
 * rule gets wrong. Locale and timezone are the platform's; only the field set
 * is ours.
 */
export function absoluteLocalTimeWithWeekday(utcIso: string): string {
  return new Date(utcIso).toLocaleString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

const pad2 = (n: number): string => String(n).padStart(2, "0");

/**
 * The live session-header "Elapsed" stat: `HH:MM:SS` since `startedAtIso`, as
 * measured at `now`. Clamped to zero rather than going negative for a
 * not-yet-started session or a skewed clock.
 */
export function formatElapsedHms(startedAtIso: string, now: Date): string {
  const totalSeconds = Math.max(
    0,
    Math.floor((now.getTime() - new Date(startedAtIso).getTime()) / 1000),
  );
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return `${pad2(hours)}:${pad2(minutes)}:${pad2(seconds)}`;
}

/**
 * The post-net summary's "Duration" stat: `HH:MM` from a whole-seconds span —
 * a closed session's duration is fixed, so it never needs a running seconds
 * digit the way the live Elapsed stat does.
 */
export function formatDurationHm(totalSeconds: number): string {
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  return `${pad2(hours)}:${pad2(minutes)}`;
}
