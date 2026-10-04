// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useNavigate } from "react-router";

import { tokens } from "../../ui/tokens/tokens";
import { Panel } from "../../ui/components/Panel";
import type { Account, Problem } from "../auth/authApi";
import { ProblemError, fetchCurrentAccount } from "../auth/authApi";
import { messageForProblem } from "../../errors/problemMessages";
import { useAuthRequest } from "../auth/useAuthRequest";
import { gateDecision } from "../consent/consentGate";
import { deleteAccount } from "./accountDeletionApi";
import { setCallsign } from "./callsignApi";
import { fetchRecentCheckIns } from "./checkInHistoryApi";
import type { CheckInHistoryEntry } from "./checkInHistoryApi";
import { exportUrl } from "./dataExportApi";
import { isPlausibleCallsign } from "./callsignCheck";
import { requestEmailChange } from "./emailChangeApi";
import { removeAvatar, updateProfile, uploadAvatar } from "./profileApi";
import { profileDestination } from "./profileGate";
import { clearQrzCredentials, setQrzCredentials } from "./qrzCredentialsApi";

/** Image types the picker offers, matching what the server's byte-sniffing
 * accepts — an OS dialog that offers a PDF only to have it refused is a worse
 * experience than not offering it. */
const AVATAR_ACCEPT = "image/png,image/jpeg,image/webp,image/gif";

/** Path prefix of avatars this instance stores itself (mirrors the backend's
 * `netroll_domain::avatar::AVATAR_PATH_PREFIX`). */
const STORED_AVATAR_PREFIX = "/avatars/";

const pageStyle: CSSProperties = {
  // The content measure every other surface shares (DESIGN.md § Layout). The
  // sections' field rows flex-wrap into it, so the page grows columns with the
  // viewport instead of staying a fixed narrow strip beside 1200px pages.
  maxWidth: "1200px",
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

// The uppercase accent-colored eyebrow that opens every section inside the
// card, paired with `dividedSectionStyle`'s border-top on every section but
// the first (task 100/104 gap analysis: section-eyebrow dividers).
const eyebrowStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--accent-ink)",
  margin: "0 0 var(--space-3)",
};

const bodyColumnStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-6)",
  padding: "var(--space-5)",
};

// Every section after the first sits below a divider line — the first
// section abuts the header band's own border instead.
const dividedSectionStyle: CSSProperties = {
  paddingTop: "var(--space-5)",
  borderTop: "1px solid var(--border)",
};

// Callsigns are radio data — mono is the one heritage nod (DESIGN.md).
const callsignChipStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  fontSize: tokens.typography.callsign.fontSize,
  fontWeight: tokens.typography.callsign.fontWeight,
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-4)",
};

const validFormatIndicatorStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
  color: "var(--staying)",
};

// Email is prose identity — plain sans, unlike the mono callsign display.
const currentEmailStyle: CSSProperties = {
  margin: "0 0 var(--space-3)",
};

const rowStyle: CSSProperties = {
  display: "flex",
  flexWrap: "wrap",
  gap: "var(--space-3)",
  alignItems: "center",
};

const inputStyle: CSSProperties = {
  flex: "1 1 160px",
  maxWidth: "420px",
  padding: "var(--space-2) var(--space-3)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  fontSize: tokens.typography.body.fontSize,
};

// Profile fields are prose, not radio data — sans, unlike the callsign
// input (mono stays the one heritage nod, reserved for callsigns). Used
// directly inside a ROW container (avatar-URL edit, email change) — the
// `flex` basis is a WIDTH there, correctly sharing the row with a button.
const profileInputStyle: CSSProperties = {
  flex: "1 1 160px",
  maxWidth: "520px",
  padding: "var(--space-2) var(--space-3)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
};

// The same visual field, but for use INSIDE a `fieldWrapStyle` `<label>`
// (Display & location, QRZ credentials): no `flex` of its own — the
// `<label>` wrapper already carries the row's width-basis, so the input
// just fills it. Reusing `profileInputStyle`'s `flex: "1 1 160px"` here
// was a real bug: nested inside a COLUMN-stacked label/container, that
// same flex-basis becomes a 160px HEIGHT instead of a width, ballooning
// every field into a ~160px box with nothing to shrink it back down.
const stackedFieldInputStyle: CSSProperties = {
  width: "100%",
  padding: "var(--space-2) var(--space-3)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
};

// The `<label>` wrapper around one stacked field: caption above input, own
// flex-basis so the ROW of fields (not the input itself) wraps/sizes.
const fieldWrapStyle: CSSProperties = {
  flex: "1 1 200px",
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
};

// Small uppercase muted caption, visible above each field (mock 6a) — was
// previously an `aria-label` only, invisible to sighted users.
const fieldCaptionStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
};

// Identity header band: sits above the divided sections, its own
// border-bottom standing in for the first section's divider.
const headerBandStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-4)",
  flexWrap: "wrap",
  padding: "var(--space-5)",
  borderBottom: "1px solid var(--border)",
  // The tonal header band every framed surface opens with; the panel clips it
  // flush to the rounded corners.
  background: "var(--head-grad)",
};

const avatarSize = "64px";

const avatarImageStyle: CSSProperties = {
  width: avatarSize,
  height: avatarSize,
  borderRadius: "var(--rounded-xl)",
  objectFit: "cover",
  flex: "0 0 auto",
};

// Gradient tile shown in place of a real avatar image — accent-tinted so it
// still reads as "this account" even with no photo set.
const avatarTileStyle: CSSProperties = {
  width: avatarSize,
  height: avatarSize,
  borderRadius: "var(--rounded-xl)",
  background: "linear-gradient(135deg, var(--accent-deep), var(--accent))",
  color: "var(--on-accent)",
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  fontSize: tokens.typography.sessionTitle.fontSizePhone,
  fontWeight: tokens.typography.wordmark.fontWeight,
  flex: "0 0 auto",
};

const identityNameRowStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  flexWrap: "wrap",
};

const identityNameStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
};

const identityCallsignChipStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  fontSize: tokens.typography.body.fontSize,
  fontWeight: tokens.typography.wordmark.fontWeight,
  color: "var(--accent-ink)",
  background: "var(--self-fill)",
  border: "1px solid var(--self-border)",
  borderRadius: "var(--rounded-sm)",
  padding: "var(--space-1) var(--space-3)",
};

const metaLineStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  marginTop: "var(--space-1)",
};

const gridMetaStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
};

const identityMainStyle: CSSProperties = {
  flex: "1 1 180px",
  minWidth: "160px",
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

const errorStyle: CSSProperties = {
  color: "var(--warn)",
  marginTop: "var(--space-3)",
};

const secondaryButtonStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-4)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const hintStyle: CSSProperties = {
  color: "var(--text-muted)",
  marginTop: "var(--space-2)",
};

// The non-destructive data-export action: an outlined link styled like the
// secondary buttons (a plain `<a download href>` needs no fetch/confirm), sitting
// above the destructive delete section per the data-rights order (export first).
const downloadLinkStyle: CSSProperties = {
  display: "inline-block",
  padding: "var(--space-2) var(--space-4)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  textDecoration: "none",
  cursor: "pointer",
};

// QRZ write-only info callout — replaces the plain muted sentence with a
// bordered, accent-tinted box (task 106).
const infoCalloutStyle: CSSProperties = {
  background: "color-mix(in srgb, var(--accent) 6%, var(--surface-2))",
  border: "1px solid var(--border)",
  borderLeft: "4px solid var(--accent)",
  borderRadius: "var(--rounded-lg)",
  padding: "var(--space-3) var(--space-4)",
  color: "var(--text-muted)",
  fontSize: tokens.typography.body.fontSize,
  lineHeight: tokens.typography.sans.lineHeight,
};

const checkInRowStyle: CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  gap: "var(--space-3)",
  alignItems: "center",
  padding: "var(--space-3) 0",
  borderTop: "1px solid var(--border)",
};

const checkInNetStyle: CSSProperties = {
  fontSize: tokens.typography.body.fontSize,
  fontWeight: 700,
};

const checkInMetaStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
};

const checkInBandStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  color: "var(--freq-text)",
};

const checkInDateStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  fontSize: tokens.typography.meta.fontSize,
  textAlign: "right",
};

const loadMoreRowStyle: CSSProperties = {
  marginTop: "var(--space-3)",
};

/**
 * The check-in-history section's accumulated state. `rows` grows by
 * appending each fetched page; `nextCursor` is `null` once the walk is done.
 */
type CheckInHistoryState =
  | { status: "loading" }
  | {
      status: "success";
      rows: CheckInHistoryEntry[];
      nextCursor: string | null;
    }
  | { status: "error"; problem?: Problem };

/**
 * Renders a wire instant for the history row. The server sends RFC 3339 (the
 * wire convention); the reader's own locale decides how a date looks.
 */
function formatCheckInDate(iso: string): string {
  const parsed = new Date(iso);
  return Number.isNaN(parsed.getTime()) ? iso : parsed.toLocaleDateString();
}

// Danger zone: a persistent red/--sync-tinted card (task 108) — the 15-minute
// undo copy is always visible, not just after the first Delete click, and
// never the design mock's incorrect "30-day" wording (task 105/109).
const dangerZoneCardStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-3)",
  alignItems: "center",
  flexWrap: "wrap",
  padding: "var(--space-4)",
  border: "1px solid var(--sync-border)",
  background: "var(--sync-fill)",
  borderRadius: "var(--rounded-lg)",
};

const dangerZoneTitleStyle: CSSProperties = {
  fontSize: tokens.typography.body.fontSize,
  fontWeight: 700,
  color: "var(--sync-text)",
};

const dangerZoneDescriptionStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  marginTop: "var(--space-1)",
};

const dangerButtonStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-4)",
  background: "transparent",
  color: "var(--sync-text)",
  border: "1px solid var(--sync-border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

// The confirm step is filled --sync-solid — the strongest destructive
// affordance, matching the red-tinted danger zone it lives in.
const confirmDeleteButtonStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-4)",
  background: "var(--sync-solid)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

/**
 * Derives a 1-2 character initials tile label from the account's display
 * name, falling back to the callsign, then to a bare "?" placeholder — used
 * by the identity header's avatar-fallback tile (task 103 gap analysis).
 */
function initialsFor(
  displayName: string | null,
  callsign: string | null,
): string {
  const source = displayName?.trim() || callsign?.trim() || "";
  if (source === "") {
    return "?";
  }
  const words = source.split(/\s+/).filter(Boolean);
  if (words.length === 1) {
    return words[0].slice(0, 2).toUpperCase();
  }
  return (words[0][0] + words[words.length - 1][0]).toUpperCase();
}

/**
 * Profile surface: the callsign set/change section plus the
 * profile section — display name, location, grid square, and avatar
 * — and account self-deletion on the same page.
 */
export function ProfilePage(): ReactElement {
  const navigate = useNavigate();

  // undefined = still loading /me; null = signed out.
  const [account, setAccount] = useState<Account | null | undefined>(
    undefined,
  );
  // The custom-avatar URL that failed to load, if any — when it matches the
  // account's current avatarUrl we fall back to the initials tile. Tracking
  // the failed URL (not a bare boolean) both prevents an error loop and lets
  // a later valid avatar recover.
  const [failedAvatarSrc, setFailedAvatarSrc] = useState<string | null>(null);
  const [displayNameInput, setDisplayNameInput] = useState("");
  const [locationInput, setLocationInput] = useState("");
  const [gridInput, setGridInput] = useState("");
  const [avatarUrlInput, setAvatarUrlInput] = useState("");
  // The avatar-URL editing capability lives behind "Change avatar" rather
  // than dominating the identity header (task 103 gap analysis).
  const [editingAvatar, setEditingAvatar] = useState(false);
  // Upload/remove are their own in-flight + error state, separate from the
  // profile PUT: a refused image must not read as "saving your profile failed".
  const [avatarBusy, setAvatarBusy] = useState(false);
  const [avatarProblem, setAvatarProblem] = useState<Problem | undefined>();
  // The callsign section's display/edit split (task 105 gap analysis) — a
  // set callsign defaults to the display chip; edit mode is opt-in via
  // "Change callsign".
  const [editingCallsign, setEditingCallsign] = useState(false);

  const prefillProfileInputs = (loaded: Account): void => {
    setDisplayNameInput(loaded.displayName ?? "");
    setLocationInput(loaded.location ?? "");
    setGridInput(loaded.grid ?? "");
    setAvatarUrlInput(loaded.avatarUrl ?? "");
  };

  useEffect(() => {
    let cancelled = false;
    fetchCurrentAccount()
      .then((result) => {
        if (!cancelled) {
          setAccount(result);
          // Prefill alongside the account so the form never renders empty
          // first. Only here and on profile-save success — a callsign
          // save's account refresh must not clobber in-progress edits.
          if (result !== null) {
            prefillProfileInputs(result);
          }
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
    const destination = profileDestination(gateDecision(account));
    if (destination === "/consent") {
      void navigate(destination, {
        replace: true,
        state: { returnTo: "/profile" },
      });
    } else if (destination !== null) {
      void navigate(destination, { replace: true });
    }
  }, [account, navigate]);

  const [callsignInput, setCallsignInput] = useState("");
  const { state, run } = useAuthRequest(() => setCallsign(callsignInput));

  useEffect(() => {
    if (state.status === "success") {
      setAccount(state.data);
      setCallsignInput("");
      setEditingCallsign(false);
    }
  }, [state]);

  // Blank-after-trim means "clear" — a blanked input does what it says.
  const fieldValue = (input: string): string | null => {
    const trimmed = input.trim();
    return trimmed === "" ? null : trimmed;
  };
  const { state: profileState, run: runProfile } = useAuthRequest(() =>
    updateProfile({
      displayName: fieldValue(displayNameInput),
      location: fieldValue(locationInput),
      grid: fieldValue(gridInput),
      avatarUrl: fieldValue(avatarUrlInput),
    }),
  );

  useEffect(() => {
    if (profileState.status === "success") {
      setAccount(profileState.data);
      // Re-sync inputs so server normalization (the canonical grid) shows.
      prefillProfileInputs(profileState.data);
    }
  }, [profileState]);

  /** True when the account's avatar is a file THIS instance stores (as opposed
   * to an externally-hosted URL) — the only case "Remove avatar" applies to. */
  const hasStoredAvatar = (account?.avatarUrl ?? "").startsWith(
    STORED_AVATAR_PREFIX,
  );

  /** Uploads a picked file, reflecting the server's account body on success.
   * The previous avatar stays on screen if the image is refused. */
  const onUploadAvatar = async (file: File): Promise<void> => {
    setAvatarBusy(true);
    setAvatarProblem(undefined);
    try {
      const updated = await uploadAvatar(file);
      setAccount(updated);
      prefillProfileInputs(updated);
      // A previously-failed avatar src must not keep forcing the initials
      // tile once a new image is in effect.
      setFailedAvatarSrc(null);
    } catch (error: unknown) {
      setAvatarProblem(
        error instanceof ProblemError ? error.problem : undefined,
      );
    } finally {
      setAvatarBusy(false);
    }
  };

  /** Removes the stored avatar, falling back to the Gravatar. */
  const onRemoveAvatar = async (): Promise<void> => {
    setAvatarBusy(true);
    setAvatarProblem(undefined);
    try {
      const updated = await removeAvatar();
      setAccount(updated);
      prefillProfileInputs(updated);
      setFailedAvatarSrc(null);
    } catch (error: unknown) {
      setAvatarProblem(
        error instanceof ProblemError ? error.problem : undefined,
      );
    } finally {
      setAvatarBusy(false);
    }
  };

  // Email-change section: its own request instance, independent of the
  // callsign and profile saves. The success payload is the normalized
  // address we mailed, so the link-sent state can name it without a stale
  // closure over the input.
  const [emailInput, setEmailInput] = useState("");
  const {
    state: emailState,
    run: runEmailChange,
    reset: resetEmail,
  } = useAuthRequest(async () => {
    const target = emailInput.trim().toLowerCase();
    await requestEmailChange(target);
    return target;
  });

  // QRZ credentials: write-only. The callsign + password are POSTed to be
  // sealed server-side; the password is NEVER fetched or prefilled — the
  // account carries only `qrzCredentialsSet`. On success we flip that boolean
  // locally and wipe the inputs (no secret lingers in component state).
  const [qrzCallsignInput, setQrzCallsignInput] = useState("");
  const [qrzPasswordInput, setQrzPasswordInput] = useState("");
  const { state: qrzSetState, run: runQrzSet } = useAuthRequest(async () => {
    await setQrzCredentials(qrzCallsignInput.trim(), qrzPasswordInput);
  });
  const { state: qrzClearState, run: runQrzClear } =
    useAuthRequest(clearQrzCredentials);

  useEffect(() => {
    if (qrzSetState.status === "success") {
      setAccount((prev) =>
        prev ? { ...prev, qrzCredentialsSet: true } : prev,
      );
      setQrzCallsignInput("");
      setQrzPasswordInput("");
    }
  }, [qrzSetState]);

  useEffect(() => {
    if (qrzClearState.status === "success") {
      setAccount((prev) =>
        prev ? { ...prev, qrzCredentialsSet: false } : prev,
      );
    }
  }, [qrzClearState]);

  // Account self-deletion: its own request instance and a local two-step
  // confirm guard so a single click can never destroy the account.
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const { state: deleteState, run: runDelete } = useAuthRequest(deleteAccount);

  useEffect(() => {
    if (deleteState.status === "success") {
      // The delete revoked every session — the user is signed out
      // everywhere. Clear local state and land on /sign-in, from which a
      // fresh magic link within the window cancels the deletion (undelete).
      setAccount(null);
      void navigate("/sign-in", { replace: true });
    }
  }, [deleteState, navigate]);

  // Recent check-ins: the caller's own self check-ins,
  // one keyset page at a time. Deliberately NOT `useAuthRequest` like every
  // other request on this page — its `run()` resets to `loading` and REPLACES
  // `data`, which is right for a refetch and wrong for "Load more": it would
  // wipe the rows already on screen.
  const [checkInHistoryState, setCheckInHistoryState] =
    useState<CheckInHistoryState>({ status: "loading" });
  // Held apart from the list state so a failed NEXT page leaves the pages
  // already fetched intact — those rows are still valid and the cursor is
  // still resumable, so the failure belongs beside the list, not instead of it.
  const [checkInPagingProblem, setCheckInPagingProblem] = useState<
    Problem | undefined
  >(undefined);
  // "Load more" is not idempotent — a second press with the same cursor
  // re-fetches and re-appends the SAME page — so the affordance is withdrawn
  // while a page request is outstanding rather than left double-clickable.
  //
  // The withdrawal is `disabled={checkInPageLoading}` on the button, and that
  // binding is the WHOLE mechanism: React dispatches no click to a disabled
  // button, even for a programmatic `dispatchEvent`. `loadMoreCheckIns`
  // deliberately carries no in-flight guard of its own — one was tried and
  // proven inert, because the loader is redefined per render and closes over
  // that render's flag, so two invocations originating from one render both
  // read `false`. If the loader ever gains a caller that is NOT
  // this disabled-gated button, that caller needs its own protection; the flag
  // below will not supply it.
  const [checkInPageLoading, setCheckInPageLoading] = useState(false);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const page = await fetchRecentCheckIns();
        if (live) {
          setCheckInHistoryState({
            status: "success",
            rows: page.items,
            nextCursor: page.nextCursor,
          });
        }
      } catch (error) {
        if (live) {
          setCheckInHistoryState({
            status: "error",
            problem: error instanceof ProblemError ? error.problem : undefined,
          });
        }
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  const loadMoreCheckIns = async (): Promise<void> => {
    if (
      checkInHistoryState.status !== "success" ||
      checkInHistoryState.nextCursor === null
    ) {
      return;
    }
    const { rows, nextCursor } = checkInHistoryState;
    setCheckInPagingProblem(undefined);
    setCheckInPageLoading(true);
    try {
      const page = await fetchRecentCheckIns(nextCursor);
      setCheckInHistoryState({
        status: "success",
        rows: [...rows, ...page.items],
        nextCursor: page.nextCursor,
      });
    } catch (error) {
      setCheckInPagingProblem(
        error instanceof ProblemError ? error.problem : undefined,
      );
    } finally {
      setCheckInPageLoading(false);
    }
  };

  if (account === undefined || account === null || account.consentRequired) {
    // Redirect effects are in flight; render nothing rather than a flash.
    return <main style={pageStyle} />;
  }

  // Fast client-side feedback only (never authoritative — the server's
  // parse_callsign is); an implausible-looking value still submits on Save
  // so the server's specific rejection reason is what the user ultimately
  // sees.
  const trimmedInput = callsignInput.trim();
  const showsPlausibilityHint =
    trimmedInput.length > 0 && !isPlausibleCallsign(trimmedInput);

  const profileFields: ReadonlyArray<{
    label: string;
    value: string;
    set: (value: string) => void;
  }> = [
    { label: "Display name", value: displayNameInput, set: setDisplayNameInput },
    { label: "Location", value: locationInput, set: setLocationInput },
    { label: "Grid square", value: gridInput, set: setGridInput },
  ];

  // Edit mode is forced open until a callsign exists — there's no display
  // chip to show yet, so "Change callsign" would have nothing to toggle.
  const callsignEditOpen = account.callsign === null || editingCallsign;

  const showAvatarImage =
    account.avatarUrl !== null && failedAvatarSrc !== account.avatarUrl;
  const initials = initialsFor(account.displayName, account.callsign);

  const hasMetaLine = Boolean(account.location) || Boolean(account.grid);

  return (
    <main style={pageStyle}>
      <h1 style={headingStyle}>Profile</h1>
      <Panel>
        {/* identity header */}
        <div style={headerBandStyle} data-identity-band>
          {showAvatarImage ? (
            // referrerPolicy avoids leaking this page's URL to whatever host
            // serves the custom avatar image.
            <img
              src={account.avatarUrl as string}
              onError={() => setFailedAvatarSrc(account.avatarUrl)}
              alt="Your avatar"
              referrerPolicy="no-referrer"
              style={avatarImageStyle}
            />
          ) : (
            // Decorative — the name right beside it already announces the
            // account, so the tile itself carries no accessible name.
            <span aria-hidden="true" style={avatarTileStyle}>
              {initials}
            </span>
          )}
          <div style={identityMainStyle}>
            <div style={identityNameRowStyle}>
              <span style={identityNameStyle}>
                {account.displayName ?? "Unnamed operator"}
              </span>
              {account.callsign !== null && (
                <span className="mono" style={identityCallsignChipStyle}>
                  {account.callsign}
                </span>
              )}
            </div>
            {hasMetaLine && (
              <div style={metaLineStyle}>
                {account.location}
                {account.location && account.grid && " · "}
                {account.grid && (
                  <span style={gridMetaStyle}>{account.grid}</span>
                )}
                {/* No "joined <date>" segment: Account carries no
                    createdAt/joinedAt field today, and adding one is outside
                    ProfilePage's scope — flagged as a gap rather than
                    fabricated. */}
              </div>
            )}
          </div>
          <button
            type="button"
            onClick={() => setEditingAvatar((value) => !value)}
            style={secondaryButtonStyle}
          >
            Change avatar
          </button>
          {editingAvatar && (
            <div style={{ flexBasis: "100%" }}>
              {/* Uploading a file is the primary path — a native picker, which
                  is what "change avatar" means to everyone. `accept` keeps the
                  OS dialog from offering files the server will refuse. */}
              <label style={fieldWrapStyle}>
                <span style={fieldCaptionStyle}>Choose an image</span>
                <input
                  type="file"
                  accept={AVATAR_ACCEPT}
                  onChange={(event) => {
                    const file = event.target.files?.[0];
                    // Reset the input so re-picking the SAME file after a
                    // failure still fires a change event.
                    event.target.value = "";
                    if (file) {
                      void onUploadAvatar(file);
                    }
                  }}
                  disabled={avatarBusy}
                  style={stackedFieldInputStyle}
                />
                <span style={hintStyle}>
                  PNG, JPEG, WebP, or GIF, up to 1&nbsp;MB.
                </span>
              </label>

              {avatarProblem !== undefined && (
                <p role="alert" style={errorStyle}>
                  {messageForProblem(avatarProblem)}
                </p>
              )}

              {/* Only offered when there is a file of ours to remove — an
                  externally-hosted URL is cleared through the URL field. */}
              {hasStoredAvatar && (
                <button
                  type="button"
                  onClick={() => void onRemoveAvatar()}
                  disabled={avatarBusy}
                  style={{ ...secondaryButtonStyle, marginTop: "var(--space-3)" }}
                >
                  Remove avatar
                </button>
              )}

              {/* The URL field stays for an externally-hosted image (the
                  original capability); uploading simply supersedes it. */}
              <div style={{ ...rowStyle, marginTop: "var(--space-4)" }}>
                <label style={fieldWrapStyle}>
                  <span style={fieldCaptionStyle}>Or link an image URL</span>
                  <input
                    type="text"
                    aria-label="Avatar URL"
                    value={avatarUrlInput}
                    onChange={(event) => setAvatarUrlInput(event.target.value)}
                    disabled={profileState.status === "loading"}
                    style={stackedFieldInputStyle}
                  />
                </label>
                <button
                  type="button"
                  onClick={() => void runProfile()}
                  disabled={profileState.status === "loading"}
                  style={primaryButtonStyle}
                >
                  Save avatar
                </button>
              </div>
            </div>
          )}
        </div>

        <div style={bodyColumnStyle}>
          <section>
            <h2 style={eyebrowStyle}>Display &amp; location</h2>
            <div
              style={{
                display: "flex",
                flexWrap: "wrap",
                gap: "var(--space-3)",
                marginBottom: "var(--space-3)",
              }}
            >
              {profileFields.map((field) => (
                <label key={field.label} style={fieldWrapStyle}>
                  <span style={fieldCaptionStyle}>{field.label}</span>
                  <input
                    type="text"
                    value={field.value}
                    onChange={(event) => field.set(event.target.value)}
                    disabled={profileState.status === "loading"}
                    style={stackedFieldInputStyle}
                  />
                </label>
              ))}
            </div>
            <button
              type="button"
              onClick={() => void runProfile()}
              disabled={profileState.status === "loading"}
              style={primaryButtonStyle}
            >
              Save profile
            </button>
            {profileState.status === "error" && (
              <p role="alert" style={errorStyle}>
                {messageForProblem(profileState.problem)}
              </p>
            )}
          </section>

          <section style={dividedSectionStyle}>
            <h2 style={eyebrowStyle}>Callsign</h2>
            {!callsignEditOpen && account.callsign !== null && (
              <div style={rowStyle}>
                <span className="mono" style={callsignChipStyle}>
                  {account.callsign}
                </span>
                <span style={validFormatIndicatorStyle}>
                  Valid format · self-asserted
                </span>
                <button
                  type="button"
                  onClick={() => setEditingCallsign(true)}
                  style={{ ...secondaryButtonStyle, marginLeft: "auto" }}
                >
                  Change callsign
                </button>
              </div>
            )}
            {callsignEditOpen && (
              <>
                <div style={rowStyle}>
                  <input
                    type="text"
                    aria-label="Callsign"
                    value={callsignInput}
                    onChange={(event) => setCallsignInput(event.target.value)}
                    disabled={state.status === "loading"}
                    style={inputStyle}
                  />
                  <button
                    type="button"
                    onClick={() => void run()}
                    disabled={state.status === "loading"}
                    style={primaryButtonStyle}
                  >
                    Save callsign
                  </button>
                  {account.callsign !== null && (
                    <button
                      type="button"
                      onClick={() => {
                        setEditingCallsign(false);
                        setCallsignInput("");
                      }}
                      style={secondaryButtonStyle}
                    >
                      Cancel
                    </button>
                  )}
                </div>
                {showsPlausibilityHint && (
                  <p style={hintStyle}>
                    That doesn&rsquo;t look like a callsign yet.
                  </p>
                )}
                {state.status === "error" && (
                  <p role="alert" style={errorStyle}>
                    {messageForProblem(state.problem)}
                  </p>
                )}
              </>
            )}
          </section>

          <section style={dividedSectionStyle}>
            <h2 style={eyebrowStyle}>Email</h2>
            {/* Email is prose identity, not radio data — sans-serif, never mono
                (mono is rationed to callsigns/frequencies, DESIGN.md). */}
            <p style={currentEmailStyle}>{account.email}</p>
            {emailState.status === "success" ? (
              <>
                <p style={hintStyle}>
                  We sent a confirmation link to {emailState.data}. Your email
                  stays {account.email} until you confirm.
                </p>
                <button
                  type="button"
                  onClick={() => resetEmail()}
                  style={secondaryButtonStyle}
                >
                  Use a different address
                </button>
              </>
            ) : (
              <>
                <div style={rowStyle}>
                  <input
                    type="email"
                    aria-label="New email"
                    value={emailInput}
                    onChange={(event) => setEmailInput(event.target.value)}
                    disabled={emailState.status === "loading"}
                    style={profileInputStyle}
                  />
                  <button
                    type="button"
                    onClick={() => void runEmailChange()}
                    disabled={emailState.status === "loading"}
                    style={primaryButtonStyle}
                  >
                    Send confirmation link
                  </button>
                </div>
                {emailState.status === "error" && (
                  <p role="alert" style={errorStyle}>
                    {messageForProblem(emailState.problem)}
                  </p>
                )}
              </>
            )}
          </section>

          <section style={dividedSectionStyle}>
            <h2 style={eyebrowStyle}>QRZ lookup credentials</h2>
            <div style={infoCalloutStyle}>
              {account.qrzCredentialsSet ? (
                <>
                  QRZ credentials are set. Stored{" "}
                  <strong style={{ color: "var(--text)" }}>write-only</strong>{" "}
                  and used only to autofill names during your nets — we never
                  display them back; re-enter to change.
                </>
              ) : (
                <>
                  No QRZ credentials set. Stored{" "}
                  <strong style={{ color: "var(--text)" }}>write-only</strong>{" "}
                  and used only to autofill names during your nets — add them
                  to enable callbook lookups.
                </>
              )}
            </div>
            <div
              style={{
                display: "flex",
                flexWrap: "wrap",
                gap: "var(--space-3)",
                marginTop: "var(--space-3)",
                marginBottom: "var(--space-3)",
              }}
            >
              <label style={fieldWrapStyle}>
                <span style={fieldCaptionStyle}>QRZ callsign</span>
                <input
                  type="text"
                  value={qrzCallsignInput}
                  onChange={(event) => setQrzCallsignInput(event.target.value)}
                  disabled={qrzSetState.status === "loading"}
                  autoComplete="username"
                  style={stackedFieldInputStyle}
                />
              </label>
              {/* Write-only: never prefilled, never fetched — the stored
                  password cannot be read back by anyone, including this page. */}
              <label style={fieldWrapStyle}>
                <span style={fieldCaptionStyle}>QRZ password</span>
                <input
                  type="password"
                  value={qrzPasswordInput}
                  onChange={(event) => setQrzPasswordInput(event.target.value)}
                  disabled={qrzSetState.status === "loading"}
                  autoComplete="new-password"
                  style={stackedFieldInputStyle}
                />
              </label>
            </div>
            <div style={rowStyle}>
              <button
                type="button"
                onClick={() => void runQrzSet()}
                disabled={qrzSetState.status === "loading"}
                style={primaryButtonStyle}
              >
                {account.qrzCredentialsSet
                  ? "Replace credentials"
                  : "Save credentials"}
              </button>
              {account.qrzCredentialsSet && (
                <button
                  type="button"
                  onClick={() => void runQrzClear()}
                  disabled={qrzClearState.status === "loading"}
                  style={secondaryButtonStyle}
                >
                  Clear
                </button>
              )}
            </div>
            {qrzSetState.status === "error" && (
              <p role="alert" style={errorStyle}>
                {messageForProblem(qrzSetState.problem)}
              </p>
            )}
            {qrzClearState.status === "error" && (
              <p role="alert" style={errorStyle}>
                {messageForProblem(qrzClearState.problem)}
              </p>
            )}
          </section>

          <section style={dividedSectionStyle}>
            <h2 style={eyebrowStyle}>Recent check-ins</h2>
            {checkInHistoryState.status === "loading" && (
              <p style={hintStyle}>Loading your recent check-ins&hellip;</p>
            )}
            {checkInHistoryState.status === "error" && (
              <p role="alert" style={errorStyle}>
                {messageForProblem(checkInHistoryState.problem)}
              </p>
            )}
            {checkInHistoryState.status === "success" &&
              (checkInHistoryState.rows.length === 0 ? (
                <p style={hintStyle}>No check-ins yet.</p>
              ) : (
                <>
                  <ul style={{ listStyle: "none", margin: 0, padding: 0 }}>
                    {checkInHistoryState.rows.map((entry, index) => (
                      // Keyed by list position, NOT by `netSessionId`: the same
                      // account can hold two self check-ins in one session (a
                      // moderator removes one — which this history still shows,
                      // since it folds no `checkin.removed` — and the
                      // participant re-checks-in), so the session id is not
                      // unique here. The wire item deliberately carries no
                      // per-check-in id to key on. The index is stable because
                      // `rows` is strictly append-only: the first page sets it
                      // and "Load more" concatenates onto the end, so an
                      // existing row never changes position.
                      <li key={index} style={checkInRowStyle}>
                        <div>
                          <div style={checkInNetStyle}>{entry.netTitle}</div>
                          <div style={checkInMetaStyle}>{entry.callsign}</div>
                        </div>
                        {/*
                          `band`/`mode` are the net's FIRST
                          connection's, so an internet-only net sends `null` for
                          both. Rendering unconditionally left an empty chip
                          beside a bare separator — punctuation with nothing to
                          separate — on the exact capability this epic exists to
                          enable.
                        */}
                        <div style={checkInMetaStyle}>
                          {entry.band !== null && (
                            <span className="mono" style={checkInBandStyle}>
                              {entry.band}
                            </span>
                          )}
                          {entry.band !== null && entry.mode !== null && " · "}
                          {entry.mode}
                          {/*
                            Band and mode are THIS check-in's
                            way in, not the net's first connection's. A way in
                            reached by name rather than by tuning — an EchoLink
                            node, a talkgroup, a reflector — has neither, and
                            the way in itself is what the row has to say. The
                            separator only appears when there is something on
                            its left to separate.
                          */}
                          {entry.via !== null && (
                            <>
                              {(entry.band !== null || entry.mode !== null) && " · "}
                              <span data-via>{entry.via}</span>
                            </>
                          )}
                        </div>
                        <span className="mono" style={checkInDateStyle}>
                          {formatCheckInDate(entry.checkedInAt)}
                        </span>
                      </li>
                    ))}
                  </ul>
                  {(checkInPagingProblem !== undefined ||
                    checkInHistoryState.nextCursor !== null) && (
                    <div style={loadMoreRowStyle}>
                      {checkInPagingProblem !== undefined && (
                        <p
                          role="alert"
                          style={{
                            ...errorStyle,
                            marginBottom: "var(--space-2)",
                          }}
                        >
                          {messageForProblem(checkInPagingProblem)}
                        </p>
                      )}
                      {checkInHistoryState.nextCursor !== null && (
                        <button
                          type="button"
                          onClick={() => void loadMoreCheckIns()}
                          disabled={checkInPageLoading}
                          style={secondaryButtonStyle}
                        >
                          Load more
                        </button>
                      )}
                    </div>
                  )}
                </>
              ))}
          </section>

          <section style={dividedSectionStyle}>
            <h2 style={eyebrowStyle}>Download your data</h2>
            <p style={hintStyle}>
              Download everything NetRoll holds about you &mdash; your
              profile, callsign, favorites, the nets you own, and your own
              check-in history &mdash; as a JSON file.
            </p>
            <div style={rowStyle}>
              <a href={exportUrl()} download style={downloadLinkStyle}>
                Download your data
              </a>
            </div>
          </section>

          <section style={dividedSectionStyle}>
            <div style={dangerZoneCardStyle}>
              <div style={{ flex: "1 1 200px" }}>
                <div style={dangerZoneTitleStyle}>Delete account</div>
                <div style={dangerZoneDescriptionStyle}>
                  Removes your profile and future access. You have 15 minutes
                  to sign back in and cancel it.
                </div>
              </div>
              {confirmingDelete ? (
                <div style={rowStyle}>
                  <button
                    type="button"
                    onClick={() => void runDelete()}
                    disabled={deleteState.status === "loading"}
                    style={confirmDeleteButtonStyle}
                  >
                    Confirm deletion
                  </button>
                  <button
                    type="button"
                    onClick={() => setConfirmingDelete(false)}
                    style={secondaryButtonStyle}
                  >
                    Keep my account
                  </button>
                </div>
              ) : (
                <button
                  type="button"
                  onClick={() => setConfirmingDelete(true)}
                  style={dangerButtonStyle}
                >
                  Delete account
                </button>
              )}
            </div>
            {deleteState.status === "error" && (
              <p role="alert" style={errorStyle}>
                {messageForProblem(deleteState.problem)}
              </p>
            )}
          </section>
        </div>
      </Panel>
    </main>
  );
}
