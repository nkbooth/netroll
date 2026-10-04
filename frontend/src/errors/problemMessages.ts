// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Shared RFC 9457 problem → user-facing message harness. Neutral (no UI, no
 * feature ownership): every surface in the app resolves error copy through
 * `messageForProblem`, which prefers the server's `detail` and falls back to
 * the slug map below. A mapped slug is an actionable, user-facing message;
 * anything unmapped, slug-less, or absent (a network failure) with no `detail`
 * degrades to the generic fallback.
 */

import type { Problem } from "../features/auth/authApi";

/**
 * Shown for any failure this harness does not recognize (unmapped slug,
 * slug-less problem, or a network error with no problem body). The canonical
 * EXPERIENCE.md fallback voice.
 */
export const GENERIC_PROBLEM_MESSAGE = "Something's off on our end — try again.";

const MESSAGES: Record<string, string> = {
  "/errors/magic-link-expired":
    "That sign-in link has expired. Request a new one below.",
  "/errors/magic-link-consumed":
    "That sign-in link was already used. Request a new one below.",
  "/errors/magic-link-invalid":
    "That sign-in link isn't valid. Request a new one below.",
  "/errors/rate-limited":
    "Too many requests just now — wait a few minutes, then try again.",
  // This used to read "That doesn't look like an email address."
  // for a slug that also covers every malformed query parameter, every
  // extractor rejection, and all six `ApiError::Validation` sites — an
  // accusation that is wrong for nearly all of them. It is a FALLBACK, and
  // deliberately generic: one slug covering two dozen causes cannot name a
  // field. The field and the remedy live in `problem.detail`, which the backend
  // always populates on this slug — and every render site
  // reads `detail` first, through `messageForProblem` below. Dated snapshot
  // 2026-08-30, re-measured since: 44 CALL SITES across 26 files
  // resolve through that one function and none hand-rolls the ordering. (It
  // said 42 across 25 and was right on 2026-08-28 — two days of drift, which is
  // exactly why the command below sits beside the number.) Call sites, not render
  // surfaces — `AdminPage`'s single call feeds seven `role="alert"` elements,
  // so surfaces number more. The command below counts call sites, which is why
  // that is the noun. Re-measure rather than trust
  // the number — from `frontend/src`:
  //   grep -rn "messageForProblem(" . --include="*.ts" --include="*.tsx" \
  //     | grep -v "\.test\." | grep -v "problemMessages.ts"
  // This string is what a `/errors/validation` rejection carrying NO `detail`
  // still shows, so it must stay true of every cause the slug covers.
  "/errors/validation":
    "The server couldn't accept that — check what you entered and try again.",
  // The one failure a reader can do NOTHING about: they opened a
  // log of a net they really did run, and it is permanently unreadable. The
  // server sends a `detail` that says so and `messageForProblem` prefers it;
  // this entry is the belt-and-braces for a `detail`-less response, and it must
  // never fall through to the generic "try again" — an instruction to repeat an
  // action that will never work.
  "/errors/unreplayable-log":
    "This net ran before nets could list more than one way to reach them, so its log can't be opened any more.",
  "/errors/net-connection-not-found":
    "That way of reaching the net isn't part of this session — reload the page and try again.",
  // Reachable only by a client that offered a way in the
  // picker deliberately hides; the copy still has to make sense to whoever sees
  // it, because "the UI prevents it" is not a guarantee.
  "/errors/connection-has-no-frequency":
    "That way in is reached by name, not by frequency — pick the radio way in instead.",
  "/errors/consent-required":
    "You'll need to accept the terms before doing that.",
  "/errors/consent-version-mismatch":
    "The terms changed while this page was open. Here's the current version — review and agree again.",
  "/errors/callsign-invalid":
    "That callsign isn't a valid format — check it and try again.",
  "/errors/callsign-taken":
    "That callsign is already reserved by another account.",
  "/errors/email-unverified":
    "Your email needs to be verified before you can reserve a callsign.",
  "/errors/grid-invalid":
    "That doesn't look like a Maidenhead grid — check it and try again.",
  "/errors/email-taken":
    "That email address is already used by another account.",
  "/errors/email-change-expired":
    "That confirmation link has expired. Request the change again from your profile.",
  "/errors/email-change-consumed":
    "That confirmation link was already used.",
  "/errors/email-change-invalid":
    "That confirmation link isn't valid. Request the change again from your profile.",
  "/errors/net-definition-invalid":
    "Some of the net details need fixing — check the highlighted field and try again.",
  "/errors/schedule-invalid":
    "That schedule needs fixing — check the highlighted field and try again.",
  "/errors/delivery-config-invalid":
    "Those delivery targets need fixing — check the highlighted field and try again.",
  "/errors/discovery-query-invalid":
    "That search filter isn't one we recognize — clear it and try again.",
  "/errors/callsign-required":
    "You'll need to reserve a callsign before creating a net.",
  "/errors/forbidden": "That net belongs to someone else — you can't change it.",
  "/errors/session-not-yet-live":
    "That session isn't live yet — you can only change the frequency once it's on the air.",
  "/errors/session-already-closed":
    "That session has already closed — its frequency is now frozen.",
  "/errors/net-definition-not-found":
    "That net no longer exists — it may have been deleted.",
  "/errors/owner-not-found": "No operator with that callsign was found.",
  "/errors/last-owner":
    "A net must keep at least one owner — add another owner before removing yourself, or delete the net.",
  "/errors/role-invalid":
    "That isn't a role we recognize — pick one from the list and try again.",
  "/errors/role-grant-not-found":
    "That operator doesn't have a role on this net to remove.",
  "/errors/signal-report-invalid":
    "That signal report isn't valid — check it and try again.",
  "/errors/staying-invalid":
    "That staying status isn't one we recognize — pick one and try again.",
  "/errors/precedence-invalid":
    "That precedence isn't one we recognize — pick one and try again.",
  "/errors/traffic-invalid":
    "That traffic count isn't valid — enter a whole number from 0 to 999.",
  "/errors/note-invalid":
    "That note is too long — shorten it and try again.",
  // Its OWN message, not the note's — an error names the field that
  // is actually at fault, and a way in is a short one-line label,
  // not prose.
  "/errors/via-invalid":
    "That way in isn't valid — keep it to one short line, or pick one of the net's own.",
  // Its OWN message, not the callsign's — the request's own callsign
  // was fine, and sending the operator to that control would be a wrong answer
  // that reads like a right one.
  "/errors/relayed-by-invalid":
    "That relaying station's callsign isn't valid — check it and try again.",
  "/errors/stale-version":
    "This entry changed — reloading the latest.",
  "/errors/lock-held":
    "Another operator is editing this entry.",
  "/errors/session-paused":
    "This net is paused — net control dropped. Wait for it to resume, or take control.",
  "/errors/control-not-stalled":
    "This net isn't paused — there's nothing to take control of right now.",
  "/errors/handoff-target-unqualified":
    "That operator can't run the net — pick a net-control or owner instead.",
  // A blocked participant's self-check-in — a clear,
  // friendly message with no PII and no raw slug.
  "/errors/account-blocked":
    "Net control has removed you from this session — you can't check back in.",
  "/errors/nothing-to-block":
    "There's no account on that entry to block — remove it without a block instead.",
  // The QRZ write-only credential surface.
  "/errors/qrz-credentials-invalid":
    "Those QRZ credentials don't look right — check them and try again.",
  "/errors/crypto-unavailable":
    "Callbook lookup isn't set up on this instance yet — an administrator needs to configure it.",
  // Per-account resource caps.
  "/errors/max-nets-per-user-reached":
    "You've reached the limit on active nets — archive one before creating another.",
  "/errors/max-owners-per-net-reached":
    "This net already has the maximum number of owners.",
  // The bounded admin surface. Note that
  // `/errors/forbidden` is NOT remapped here — it already carries net-ownership
  // copy that every net surface depends on; the admin page maps its own 403.
  "/errors/account-disabled":
    "This account has been disabled by an administrator.",
  "/errors/account-not-found": "No account with that identifier was found.",
  "/errors/abuse-report-not-found":
    "That report is no longer in the queue — someone may have resolved it already.",
  "/errors/cannot-disable-self":
    "You can't disable your own account — ask another administrator.",
};

/**
 * Every slug this harness has curated copy for.
 *
 * Derived from the map rather than typed out again: a hand-maintained list
 * beside a map goes stale silently, and a structural guard that iterates the
 * stale list stops covering the entries added since — which is exactly how a
 * new slug's copy could be deleted with nothing turning red.
 */
export const KNOWN_PROBLEM_TYPES: readonly string[] = Object.keys(MESSAGES);

const lookup = (type: string | undefined): string | undefined =>
  type !== undefined ? MESSAGES[type] : undefined;

const LOCK_HELD = "/errors/lock-held";

// Slugs whose `detail` is NOT copy written for an operator, so preferring it
// makes the message worse rather than better. Measured first-hand 2026-08-28
// by tracing every `detail`-carrying `ApiError` variant in
// `http/problem.rs` to the `Display` it embeds. Re-verify rather than trust:
// - `/errors/lock-held` — a BARE CALLSIGN ("K4ABC") with no sentence around
// it (`http/problem.rs`, the `LockHeld` arm; constructed in
// `http/net_sessions.rs`). Composed below instead of shown raw.
// - `/errors/staying-invalid` — "unrecognized staying token: <echo>"
// (`netroll-domain/src/check_in.rs:225-229`).
// - `/errors/precedence-invalid` — "unrecognized precedence token: <echo>"
// (`netroll-domain/src/check_in.rs:308-312`).
// The latter two name a TYPE, which is exactly what the copy rule
// rejects; they are wrong AT SOURCE and are recorded for a backend fix.
// Excluding them here is the render-side stopgap, not the cure — drop them
// from this set once those two strings become sentences.
const DETAIL_IS_NOT_USER_COPY: ReadonlySet<string> = new Set([
  LOCK_HELD,
  "/errors/staying-invalid",
  "/errors/precedence-invalid",
]);

/** A `detail` a reader can be shown — absent and `""` mean the same thing. */
const usableDetail = (problem: Problem | undefined): string | undefined =>
  problem?.detail !== undefined && problem.detail !== ""
    ? problem.detail
    : undefined;

/**
 * Resolve a problem to the message a surface should show.
 *
 * Order: a slug in the not-user-copy set falls to the map (with
 * `/errors/lock-held` composing its holder callsign into a sentence first);
 * any other non-empty `detail` wins; otherwise the slug map, which itself
 * tails into `GENERIC_PROBLEM_MESSAGE`.
 *
 * `detail` wins because it is the only part of a response that can name WHICH
 * field failed and what would make the value acceptable. The map is keyed by
 * slug, and a slug such as `/errors/validation` covers two dozen causes at
 * once, so its copy is necessarily generic. An empty-string `detail` counts as
 * absent: rendering it would leave a `role="alert"` announcing nothing.
 */
export function messageForProblem(problem: Problem | undefined): string {
  const slug = problem?.type;
  const detail = usableDetail(problem);
  if (slug !== undefined && DETAIL_IS_NOT_USER_COPY.has(slug)) {
    // This sentence lived at `CheckInDetailModal` and
    // moved here because `SelfCheckInControl` receives the same rejection and
    // would otherwise render the bare callsign. Two call sites is below the
    // extraction threshold — it moves because the knowledge "this slug's
    // detail is a token" belongs beside the slug→copy map it corrects, and
    // because "every surface resolves through one function" has to have no
    // exceptions to be checkable.
    return slug === LOCK_HELD && detail !== undefined
      ? `${detail} is editing this entry.`
      : messageForProblemType(slug);
  }
  return detail ?? messageForProblemType(slug);
}

/**
 * Resolve a problem `type` slug straight to its curated message, ignoring any
 * `detail`. The fallback half of `messageForProblem`; call it directly only
 * where a surface has no `Problem` in hand, or is deliberately overriding.
 */
export function messageForProblemType(type: string | undefined): string {
  return lookup(type) ?? GENERIC_PROBLEM_MESSAGE;
}
