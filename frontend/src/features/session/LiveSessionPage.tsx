// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useLocation, useParams, useNavigate } from "react-router";
import { useStore } from "zustand";

import { messageForProblem } from "../../errors/problemMessages";
import {
  rfConnection,
  sessionWaysIn,
} from "../nets/connectionPresentation";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { useCurrentAccount } from "../auth/useCurrentAccount";
import { Breadcrumb } from "../../ui/components/Breadcrumb";
import { Panel } from "../../ui/components/Panel";
import { StatTile } from "../../ui/components/StatTile";
import { ResponsiveList } from "../../ui/layout/ResponsiveList";
import { formatDurationHm, formatElapsedHms } from "../../ui/util/humanizeTime";
import { ConnectionStatus } from "./ConnectionStatus";
import { ReplayingState } from "./ReplayingState";
import { RosterColumnHeads } from "./RosterColumnHeads";
import { RosterEntry } from "./RosterEntry";
import { CheckInDetailModal } from "./CheckInDetailModal";
import { StartNetControl } from "./StartNetControl";
import { ClaimControl } from "./ClaimControl";
import { HandoffControl } from "./HandoffControl";
import type { HandoffTarget } from "./HandoffControl";
import { CloseNetControl } from "./CloseNetControl";
import { QuickAddRow } from "./QuickAddRow";
import { KeycapLegend } from "./KeycapLegend";
import { FrequencyControl } from "./FrequencyControl";
import { ViaStampControl } from "./ViaStampControl";
import { ReorderControl } from "./ReorderControl";
import { WorkedGroup } from "./WorkedGroup";
import { WorkedSinkToggle } from "./WorkedSinkToggle";
import { NetNotePanel } from "./NetNotePanel";
import { RoleManagementPanel } from "./RoleManagementPanel";
import { canViewerDo } from "./capabilities";
import { exportUrl, listRoles, setWorkedStation } from "./sessionApi";
import { buildRoster, countDistinctStates, displayedConnection } from "./sessionStore";
import type { DisplayRosterEntry } from "./sessionStore";
import { useSessionStream } from "./useSessionStream";
import { tokens } from "../../ui/tokens/tokens";

/** True when a keydown target is a field the operator is editing (never hijack
 * it) — the SAME guard `QuickAddRow`'s `n` hotkey uses. */
function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }
  const tag = target.tagName;
  return (
    tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable
  );
}

/**
 * The owner-facing live-session page (route `/net-sessions/:id`). It loads
 * the folded snapshot, opens the self-recovering live stream, and renders the
 * connection status, the recovery banner, the folded summary, the live
 * roster, and thin owner Start/Close controls. Signed-out visitors
 * self-gate to `/sign-in`; the server remains the real authority.
 */

const pageStyle: CSSProperties = {
  // The content measure the shell chrome shares (DESIGN.md § Layout).
  maxWidth: "1200px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
};

/** The session-header band (design handoff `2b`): eyebrow + title + freq pill
 * on the left, the Live badge + Elapsed/Checked-in stat pair right-aligned —
 * the same pattern as the participant view's header, "you are NCS" eyebrow.
 * Sits flush against the panel frame on the tonal header gradient; the panel
 * clips it, so it needs no negative-margin compensation. */
const sessionHeaderStyle: CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  gap: "var(--space-4)",
  flexWrap: "wrap",
  alignItems: "flex-start",
  padding: "var(--space-5) var(--space-row-x)",
  background: "var(--head-grad)",
  borderBottom: "1px solid var(--border)",
};

const eyebrowStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: 800,
  letterSpacing: "0.11em",
  textTransform: "uppercase",
  color: "var(--accent-ink)",
};

const titleStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  margin: "3px 0 0",
};

const freqStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
  marginTop: "var(--space-3)",
  color: "var(--freq-text)",
  background: "var(--freq-fill)",
  border: "1px solid var(--freq-border)",
  borderRadius: "var(--rounded-freq)",
  padding: "var(--space-2) var(--space-3)",
  fontSize: "14px",
};

const headerRightStyle: CSSProperties = { display: "flex", flexDirection: "column", alignItems: "flex-end" };

const statPairStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-4)",
  marginTop: "var(--space-3)",
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  flexWrap: "wrap",
};

/** The "Net closed · logged" eyebrow (design handoff `4a`) — the `--staying`
 * green replaces the live eyebrow's accent-ink, with a checkmark icon so the
 * closed state is never color alone. */
const closedEyebrowStyle: CSSProperties = {
  ...eyebrowStyle,
  color: "var(--staying)",
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
};

const closedMetaStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  marginTop: "var(--space-2)",
};

/** The post-net export header buttons (design handoff `4a`) — promoted from
 * plain download links to icon buttons in the header's top-right, one filled
 * (CSV) and one outlined (ADIF), matching the mock. */
const exportButtonBaseStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
  padding: "var(--space-2) var(--space-3)",
  borderRadius: "var(--rounded-md)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
  textDecoration: "none",
};

const exportButtonPrimaryStyle: CSSProperties = {
  ...exportButtonBaseStyle,
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  border: "none",
};

const exportButtonSecondaryStyle: CSSProperties = {
  ...exportButtonBaseStyle,
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
};

/** The post-net summary's stat-tile row (design handoff `4a`): Check-ins /
 * Traffic passed / Duration / States-provinces, reusing the shared `StatTile`
 * primitive verbatim. */
const statTileRowStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-3)",
  marginTop: "var(--space-4)",
  flexWrap: "wrap",
};

/** A download-cloud glyph for the CSV/ADIF export buttons. */
function ExportIcon(): ReactElement {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      aria-hidden="true"
    >
      <path d="M12 3v12M8 11l4 4 4-4M5 21h14" />
    </svg>
  );
}

/** The checkmark icon carrying the closed-eyebrow's `--staying` green
 * treatment (color + icon + label, never color alone). */
function ClosedCheckIcon(): ReactElement {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.6"
      aria-hidden="true"
    >
      <path d="M20 6 9 17l-5-5" />
    </svg>
  );
}

const listStyle: CSSProperties = { listStyle: "none", padding: 0, margin: 0 };

/** The merged operator toolbar strip (design handoff `2b`) — Set-frequency +
 * the keycap legend in one visual bar. See the locked decision note at its
 * call site for why the mock's Round chip / Pause-net button are omitted. */
const toolbarStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  flexWrap: "wrap",
  padding: "var(--space-3) var(--space-row-x)",
  background: "var(--surface-2)",
  borderBottom: "1px solid var(--border)",
};

const errorStyle: CSSProperties = { color: "var(--sync-text)" };

/** The console panel: header band and toolbar strip run flush to its edges,
 * while everything below them (roster, controls, modals) sits in this padded
 * body. */
const panelStyle: CSSProperties = { margin: "var(--space-4) 0" };

const panelBodyStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-row-x)",
};

/**
 * The live-session page component. A thin param-reading shell over
 * `LiveSessionPageForSession`, keyed on the route's `id`: react-router reuses
 * the SAME element across a param-only navigation (e.g. Start Net's re-run
 * creates a new session and navigates to its id on this same route), so a
 * `key` change is what forces React to fully remount — and therefore fully
 * reset — the inner component's store/phase/stream instead of carrying stale
 * state from the previous session into the new one.
 */
export function LiveSessionPage(): ReactElement {
  const { id } = useParams();
  return <LiveSessionPageForSession key={id ?? ""} sessionId={id ?? ""} />;
}

interface LiveSessionPageForSessionProps {
  readonly sessionId: string;
}

/** The actual page body, mounted fresh per `sessionId` by its `key`ed parent. */
function LiveSessionPageForSession({
  sessionId,
}: LiveSessionPageForSessionProps): ReactElement {
  const navigate = useNavigate();
  const location = useLocation();
  const { store, phase, resync } = useSessionStream(sessionId);

  // The PURE transport state (catching-up/live/out-of-sync) from the socket
  // controller; net-paused is DERIVED, never stored here.
  const transport = useStore(store, (s) => s.connection);
  const session = useStore(store, (s) => s.session);
  const meta = useStore(store, (s) => s.meta);
  const pending = useStore(store, (s) => s.pending);
  const locks = useStore(store, (s) => s.locks);
  // Surface the stalled control-status as net-paused for render.
  const connection = displayedConnection(transport, session.controlState);
  const stalled = session.controlState === "stalled";
  // The viewer's own role gates which affordances RENDER — a
  // granted Relay's console shows the add-only quick-add and hides the
  // edit/reorder/worked/net-note/frequency/close/role-management controls it
  // lacks the capability for. UX ONLY: every mutation is still server-enforced
  // This only avoids showing a control that would just 403.
  const viewerRole = meta?.viewerRole ?? null;
  // Roster/frequency WRITE affordances are additionally gated on
  // NOT stalled — the roster is frozen while the net is paused (UX only; the
  // server refuses every such write with 409 session-paused regardless).
  const canReorder = canViewerDo(viewerRole, "reorder-roster") && !stalled;
  const canSetWorked = canViewerDo(viewerRole, "set-worked-station") && !stalled;
  const canSetOrderMode = canViewerDo(viewerRole, "set-roster-order-mode") && !stalled;
  const canAnnotate = canViewerDo(viewerRole, "annotate-session") && !stalled;
  const canEditCheckIn = canViewerDo(viewerRole, "edit-check-in") && !stalled;
  const canRunSession = canViewerDo(viewerRole, "run-session");
  const canManageRoles = canViewerDo(viewerRole, "manage-roles");
  const canSetStaffFields = canViewerDo(viewerRole, "edit-staff-fields");
  // CSV/ADIF export is an NCS/owner (NetControl-floor) act. The
  // UI only surfaces the download links once the session is CLOSED (the post-net
  // use case); the server enforces the capability regardless of lifecycle.
  const canExport = canViewerDo(viewerRole, "export-session");
  // The involuntary-claim rescue capability (Logger floor). The
  // ClaimControl affordance only RENDERS while the net is stalled.
  const canClaimControl = canViewerDo(viewerRole, "claim-control");
  // Only the CURRENT
  // active NCS may voluntarily hand off — compare the viewer's own account id
  // (never a client-asserted role) against the folded `activeNcsAccountId`.
  const { account } = useCurrentAccount();
  const isActiveNcs =
    account !== null && session.activeNcsAccountId === account.id;
  // The check-in currently open in the detail modal, or null. Its
  // id is passed to buildRoster so THIS operator's own edit row is not flagged
  // "locked by another".
  const [editingKey, setEditingKey] = useState<string | null>(null);
  // The roster row currently selected as the `w` hotkey target,
  // set on row click/focus. `null` until the operator picks a row.
  const [selectedCheckInId, setSelectedCheckInId] = useState<string | null>(null);
  // A refused worked-station toggle (403/404/409), surfaced as an alert
  // (review finding: the toggle used to swallow every failure silently).
  const [workedStationProblem, setWorkedStationProblem] = useState<Problem | undefined | null>(
    null,
  );
  // The eligible NetControl-tier handoff targets: explicit session-scoped
  // grants at NetControl rank or
  // above, excluding the caller. Sourced from `listRoles` — the SAME
  // NetControl-floor read `ManageRoles` already uses (`RunSession` shares its
  // rank), so it works for a granted-NetControl active NCS too, not only a
  // definition owner. A co-owner who was never explicitly granted a session
  // role (owners resolve implicitly, not via the grants table) will not appear
  // here — a known, documented limitation.
  const [handoffTargets, setHandoffTargets] = useState<HandoffTarget[]>([]);
  const [handoffTargetsProblem, setHandoffTargetsProblem] = useState<
    Problem | undefined | null
  >(null);
  useEffect(() => {
    if (!isActiveNcs || !canRunSession || stalled || account === null) {
      setHandoffTargets([]);
      return;
    }
    let cancelled = false;
    void listRoles(sessionId)
      .then((grants) => {
        if (cancelled) {
          return;
        }
        const eligible = grants
          .filter(
            (grant) =>
              grant.accountId !== account.id && canViewerDo(grant.role, "run-session"),
          )
          .map((grant) => ({
            accountId: grant.accountId,
            callsign: grant.callsign ?? grant.accountId,
          }));
        setHandoffTargets(eligible);
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setHandoffTargetsProblem(error instanceof ProblemError ? error.problem : undefined);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [isActiveNcs, canRunSession, stalled, account, sessionId]);
  // A periodic tick so a `locks` entry that outlives its own `expiresAt`
  // (a known staleness gap: a lost release frame after a
  // broadcast-channel lag, or a resumed WS connection that never re-syncs
  // lock state) self-clears within a bounded window rather than needing an
  // unrelated roster/pending/locks change to re-render (see `isLockExpired`
  // in sessionStore.ts).
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 5_000);
    return () => window.clearInterval(id);
  }, []);
  // Memoize on the stable roster/pending/locks refs — a selector returning a
  // fresh array would loop useSyncExternalStore.
  const roster = useMemo(
    () =>
      buildRoster(
        session.roster,
        pending,
        locks,
        editingKey,
        now,
        session.workingCheckInId,
        // A row's `via` LABEL is resolved against the session's own
        // connections, here, so a mid-net frequency move re-labels every row
        // that came in on it. The parameter is REQUIRED — it had a `[]` default
        // and this comment used to say omitting it "renders no way in at all",
        // which was wrong in the dangerous direction: an empty set resolves
        // every recorded `via` to UNRESOLVABLE_VIA_LABEL, so every row would
        // have claimed the net had LOST the way it came in on.
        session.connections,
      ),
    [
      session.roster,
      pending,
      locks,
      editingKey,
      now,
      session.workingCheckInId,
      session.connections,
    ],
  );
  // The SERVER has already sunk the worked stations when the mode is
  // on, so this split changes NO order — it only groups the trailing worked run
  // so it can collapse. With the mode off nothing is grouped and the roster
  // renders exactly as it always has (a partition here would be a client sort).
  //
  // Gated on `isLive`, matching the toggle's own gate below: a closed session
  // keeps the mode it was run under (the log records what was in force) but
  // offers no way to expand the group, so grouping there would hide the whole
  // roster on the page that exists to display it.
  //
  // The entry holding the working cursor is EXEMPT from the worked group. Its
  // `worked` flag is monotonic on the server, so from the second round on
  // the station being worked RIGHT NOW carries it; filing that row into the
  // collapsed block would hide the one station the NCS is talking to. The
  // server's own partition exempts the same entry, so this keeps the two splits
  // agreeing rather than introducing a second rule.
  const isLive = session.lifecycle === "live";
  const workedSink = isLive && session.rosterOrderMode === "worked-sink";
  const isSunk = (entry: DisplayRosterEntry): boolean => entry.worked && !entry.working;
  const unworkedRows = workedSink ? roster.filter((entry) => !isSunk(entry)) : roster;
  const workedRows = workedSink ? roster.filter(isSunk) : [];

  const editingEntry: DisplayRosterEntry | null =
    editingKey === null ? null : (roster.find((r) => r.key === editingKey) ?? null);

  // The `w`-hotkey selection must not outlive its row (review finding): a
  // checkin.removed delta, or a pending row's key flipping to its real
  // checkInId on reconciliation, would otherwise leave `selectedCheckInId`
  // pointing at a key no longer present — and a later `w` press would
  // silently 404 against a check-in that no longer exists.
  useEffect(() => {
    if (selectedCheckInId !== null && !roster.some((entry) => entry.key === selectedCheckInId)) {
      setSelectedCheckInId(null);
    }
  }, [roster, selectedCheckInId]);

  // Guards against overlapping `/worked-station` POSTs: a fast double
  // press/click before the first request settles
  // would race the still-stale `session.workingCheckInId` closure value and
  // could flip-flop the cursor (set-then-immediately-clear). Neither the
  // hotkey nor the inline control disables itself while busy, so the guard
  // lives here — the single call site both share.
  const workingToggleInFlightRef = useRef(false);

  // Toggle the working cursor on a row: send `null` when the row is
  // ALREADY working (toggle-off = complete + idle), else its id. The
  // authoritative `station.worked-set` delta then folds for every console.
  const toggleWorking = useCallback(
    async (key: string): Promise<void> => {
      if (workingToggleInFlightRef.current) {
        return;
      }
      workingToggleInFlightRef.current = true;
      const target = key === session.workingCheckInId ? null : key;
      setWorkedStationProblem(null);
      try {
        const summary = await setWorkedStation(sessionId, target);
        store.getState().seedFromSnapshot(summary);
      } catch (error: unknown) {
        // A refused toggle (403 non-NCS, 404 stale target, 409 closed
        // session) never produces a WS delta to reconcile from — surface it
        // so the operator isn't left believing the click did nothing
        // (review finding; matches CheckInDetailModal/NetNotePanel's pattern).
        setWorkedStationProblem(error instanceof ProblemError ? error.problem : undefined);
      } finally {
        workingToggleInFlightRef.current = false;
      }
    },
    [session.workingCheckInId, sessionId, store],
  );

  // The document-level `w` hotkey — guarded EXACTLY like QuickAddRow's `n`:
  // never while typing in a field or with a modifier held. It toggles the
  // working cursor on the selected/focused roster row. Active only while live.
  useEffect(() => {
    if (session.lifecycle !== "live") {
      return;
    }
    // The `w` working-cursor hotkey is an NCS act (SetWorkedStation) — a Relay
    // never arms it. The server 403s regardless.
    if (!canSetWorked) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key !== "w" && event.key !== "W") {
        return;
      }
      if (event.altKey || event.ctrlKey || event.metaKey) {
        return;
      }
      if (isEditableTarget(event.target)) {
        return;
      }
      if (selectedCheckInId === null) {
        return;
      }
      // OS auto-repeat resends keydown with `repeat: true` while a key is
      // held; unlike `n` (harmless to re-fire — it just moves focus), `w`
      // triggers a network mutation, so a held key must not flood the
      // endpoint with overlapping toggles.
      if (event.repeat) {
        return;
      }
      event.preventDefault();
      void toggleWorking(selectedCheckInId);
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [session.lifecycle, selectedCheckInId, toggleWorking, canSetWorked]);

  useEffect(() => {
    if (phase.status === "signed-out") {
      // Carry `returnTo` so the sign-in flow returns the operator to this live
      // page after authenticating — the gate-and-return pattern the participant
      // self-check-in flow relies on, wired on the live pages too.
      void navigate("/sign-in", {
        replace: true,
        state: { returnTo: location.pathname },
      });
    }
  }, [phase.status, navigate, location.pathname]);

  if (phase.status === "loading" || phase.status === "signed-out") {
    // Loading the snapshot or redirecting a signed-out visitor: render the
    // shell rather than flash partial content.
    return <main style={pageStyle} />;
  }

  if (phase.status === "error") {
    return (
      <main style={pageStyle}>
        <p role="alert" style={errorStyle}>
          {messageForProblem(phase.problem)}
        </p>
      </main>
    );
  }

  const isClosed = session.lifecycle === "closed";
  const definitionTitle = meta?.definition.title ?? "Live session";
  // Band/mode moved off the snapshot's top level and into its
  // connection set, so the ONE presenter that already renders a connection set
  // answers here too rather than a second, parallel formatter growing beside it.
  const wayIn = sessionWaysIn(session.connections);
  const rfWay = rfConnection(session.connections);
  const netMode = rfWay?.mode ?? undefined;
  const bandCategoryLabel =
    meta === null
      ? "Nets"
      : `${rfWay?.band ?? "Internet"} · ${meta.definition.netCategory}`;
  // The post-net summary's derived values (design handoff `4a`). The NCS
  // callsign has no dedicated field on the folded summary — it is read off
  // the roster row whose `addedBy` matches the folded `activeNcsAccountId`
  // (the same account-id comparison `isActiveNcs` already uses above).
  const ncsCallsign = roster.find((r) => r.addedBy === session.activeNcsAccountId)?.callsign;
  const trafficPassed = roster.reduce((sum, r) => sum + (r.traffic ?? 0), 0);
  const statesCount = countDistinctStates(roster.map((r) => r.location));
  // The folded SessionState carries `startedAt`/`closedAt` but not a
  // pre-computed duration (that field lives only on the wire summary body,
  // never folded) — derive the post-net summary's Duration stat directly.
  const durationSeconds =
    session.startedAt !== null && session.closedAt !== null
      ? Math.max(
          0,
          Math.round(
            (new Date(session.closedAt).getTime() - new Date(session.startedAt).getTime()) / 1000,
          ),
        )
      : 0;

  // One row renderer for both groups — the worked rows render exactly as they
  // always have (0.55 + the green tick), just inside the collapsed group.
  const renderRosterRow = (entry: DisplayRosterEntry): ReactElement => (
    <RosterEntry
      key={entry.key}
      entry={entry}
      // The live operator console keeps the full staff surface
      // (`showSource`: report/staying/precedence/lock/corrections/
      // working-cursor/edit). The post-net summary (closed) is a frozen
      // historical record — none of the interactive/in-progress
      // affordances apply, so it opts into just the badge/report/
      // precedence cells plus an absolute "Checked" time instead of the
      // live relative "Heard" phrase (design handoff `4a`, task 92/95).
      showSource={isLive}
      showSourceBadge={isClosed}
      showReport={isClosed}
      showPrecedence={isClosed}
      // Staying is passed WHEREVER report and precedence are.
      // The closed summary opting into those two but not this one would leave it
      // the single roster in the product hiding a field every other roster shows
      // — "uniform" failing on the surface nobody demoes.
      showStaying={isClosed}
      heardAbsolute={isClosed}
      // Edit is Logger+ (EditCheckIn); the worked cursor + row selection
      // are NCS (SetWorkedStation). A Relay sees neither affordance
      // — the server enforces both regardless.
      onEdit={isLive && canEditCheckIn ? () => setEditingKey(entry.key) : undefined}
      onSetWorking={isLive && canSetWorked ? (key) => void toggleWorking(key) : undefined}
      onSelect={isLive && canSetWorked ? setSelectedCheckInId : undefined}
      selected={entry.key === selectedCheckInId}
    />
  );

  return (
    <main style={pageStyle}>
      <Breadcrumb
        items={[
          { label: "Nets", href: "/" },
          { label: bandCategoryLabel },
          { label: `${definitionTitle} · NCS console` },
        ]}
      />
      <Panel style={panelStyle}>
        <div style={sessionHeaderStyle} data-session-header>
        <div>
          {isClosed ? (
            <div style={closedEyebrowStyle}>
              <ClosedCheckIcon />
              Net closed · logged
            </div>
          ) : (
            <div style={eyebrowStyle}>{isLive ? "Live net · you are NCS" : "Net session"}</div>
          )}
          <h2 style={titleStyle}>{definitionTitle}</h2>
          {isClosed ? (
            // The closed-session meta line (design handoff `4a`): date/time
            // range, NCS callsign, frequency, band/mode — replacing the live
            // freq pill with a plain text line (a closed net has no ongoing
            // frequency to advertise as a pill).
            <p style={closedMetaStyle}>
              {session.startedAt !== null && (
                <>{new Date(session.startedAt).toLocaleDateString()} · </>
              )}
              {session.startedAt !== null && session.closedAt !== null && (
                <>
                  {new Date(session.startedAt).toLocaleTimeString()}–
                  {new Date(session.closedAt).toLocaleTimeString()} ·{" "}
                </>
              )}
              {ncsCallsign !== undefined && (
                <>
                  NCS{" "}
                  <span className="mono" style={{ color: "var(--text)" }}>
                    {ncsCallsign}
                  </span>{" "}
                  ·{" "}
                </>
              )}
              <span className="mono" style={{ color: "var(--freq-text)" }}>
                {wayIn}
              </span>
            </p>
          ) : (
            meta !== null && (
              <div style={freqStyle}>
                <span className="mono" style={{ fontSize: tokens.typography.callsign.fontSize, fontWeight: 800 }}>
                  {wayIn}
                </span>
              </div>
            )
          )}
        </div>
        {isClosed ? (
          // Post-net export, promoted to header icon-buttons (design handoff
          // `4a`). Owner/NCS-only (ExportSession); the server enforces the
          // capability regardless — this only hides buttons a lower
          // role couldn't use.
          canExport && (
            <div style={{ display: "flex", gap: "var(--space-2)" }}>
              <a download href={exportUrl(sessionId, "csv")} style={exportButtonPrimaryStyle}>
                <ExportIcon />
                Download CSV
              </a>
              <a download href={exportUrl(sessionId, "adif")} style={exportButtonSecondaryStyle}>
                <ExportIcon />
                Download ADIF
              </a>
            </div>
          )
        ) : (
          <div style={headerRightStyle}>
            <ConnectionStatus connection={connection} />
            <div style={statPairStyle}>
              {session.startedAt !== null && (
                <span>
                  Elapsed{" "}
                  <b className="mono" style={{ color: "var(--text)" }}>
                    {formatElapsedHms(session.startedAt, new Date(now))}
                  </b>
                </span>
              )}
              <span>
                Checked in <b style={{ color: "var(--text)" }}>{session.roster.length}</b>
              </span>
            </div>
          </div>
        )}
        {isClosed && (
          <div style={statTileRowStyle}>
            <StatTile value={String(session.roster.length)} label="Check-ins" />
            <StatTile value={String(trafficPassed)} label="Traffic passed" />
            <StatTile value={formatDurationHm(durationSeconds)} label="Duration" mono />
            <StatTile value={String(statesCount)} label="States / provinces" />
          </div>
        )}
      </div>

      {/* The merged operator toolbar strip (design handoff `2b`): Set-frequency
          + the n/w keycap legend in ONE visual bar. The mock also shows a
          "Round N · roll-call order" chip and a manual "Pause net" button —
          both are explicitly OMITTED (locked decision): net-paused is derived
          from presence, never manually toggled, and this codebase has no
          roll-call-round tracking to back a Round chip. Frequency is a roster/
          frequency write — frozen while stalled — so it drops out of the
          strip in that state; the keycap legend stays (pure discoverability,
          no write). */}
      {isLive && (
        <div style={toolbarStyle} data-toolbar>
          {canRunSession && !stalled && (
            <FrequencyControl sessionId={sessionId} connections={session.connections} />
          )}
          {/* The way-in stamp. Not itself a write — it never leaves
              the browser — but it governs the quick-add, which IS frozen while
              stalled, so it drops out of the strip with the thing it acts on
              rather than sitting there claiming to set something unreachable.
              It hands the stamp to nobody: `QuickAddRow` reads it from the same
              store, so there is one path to the value and one authority. */}
          {!stalled && <ViaStampControl store={store} connections={session.connections} />}
          <KeycapLegend />
        </div>
      )}

      <div style={panelBodyStyle}>
      {/* ReplayingState handles the TRANSPORT degradations (catching-up/
          out-of-sync); net-paused is a control-status render, not a transport
          resync trigger — so it reads the pure transport, not the derived value. */}
      <ReplayingState connection={transport} onResync={resync} />

      {/* Post-net export now lives as header icon-buttons in
          the closed-state session header above (design handoff `4a`) — this
          spot previously held a plain `<nav>` download-link bar. */}

      {/* The pinned quick-add is at the TOP of the roster (never behind a
          button) so a rapid-fire commit-clear-refocus loop stays in one place
        it only exists while the session is live. */}
      {/* The quick-add is present for every staff console (LogCheckIn = Relay+).
          A Relay's row HIDES the staff-only Report field. Frozen
          while stalled — the roster is suspended until resume/
          claim/auto-close; the server 409s an add regardless (UX only). */}
      {isLive && !stalled && (
        <ResponsiveList>
          <QuickAddRow
            sessionId={sessionId}
            store={store}
            mode={netMode}
            canSetReport={canSetStaffFields}
          />
        </ResponsiveList>
      )}

      {/* The in-console role-management panel — self-gates to
          ManageRoles holders (NCS/Owner). Not lifecycle-gated: role grants are a
          plain table, available whenever the console loads. */}
      {canManageRoles && (
        <RoleManagementPanel sessionId={sessionId} viewerRole={viewerRole} />
      )}

      {/* NCS "Order by precedence" — the authoritative roster.reordered delta
          folds for every console; seeding the returned summary reflects it here
          immediately. Server enforces the NCS gate; a Relay never
          sees the control. */}
      {/* The worked-sink toggle rides the same NCS gate. The mode is
          SERVER state — the returned summary seeds it, and the WS delta folds it
          for every other console. */}
      {isLive && (canReorder || canSetOrderMode) && (
        <div
          style={{
            margin: "var(--space-2) 0",
            display: "flex",
            gap: "var(--space-3)",
            flexWrap: "wrap",
            alignItems: "flex-start",
          }}
        >
          {canReorder && (
            <ReorderControl
              sessionId={sessionId}
              onReordered={(summary) => store.getState().seedFromSnapshot(summary)}
            />
          )}
          {canSetOrderMode && (
            <WorkedSinkToggle
              sessionId={sessionId}
              mode={session.rosterOrderMode}
              onModeSet={(summary) => store.getState().seedFromSnapshot(summary)}
            />
          )}
        </div>
      )}

      {/* The net-level note panel — session-scoped, live + operator
          only; the server enforces the Logger+ gate. A Relay lacks
          AnnotateSession, so the panel is hidden. */}
      {isLive && canAnnotate && (
        <NetNotePanel
          sessionId={sessionId}
          netNote={session.netNote}
          onSaved={(summary) => store.getState().seedFromSnapshot(summary)}
        />
      )}

      {/* A refused worked-station toggle (review finding — was silently
          swallowed): surfaced the same way every other write path in this
          story surfaces one. */}
      {workedStationProblem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(workedStationProblem)}
        </p>
      )}

      {/* aria-live announces new/changed check-ins to assistive tech.
          Wrapped in ResponsiveList: the roster's fixed-width columns
          scroll horizontally inside THIS container on a narrow desktop
          window, never the page body — the fixed columns blowing out the
          whole page was a real bug (design-handoff column widths reused
          without the existing scroll-containment primitive). */}
      <ResponsiveList>
        {/* The head strip mirrors the row flags below it, and only appears
            when there are rows for it to label. */}
        {roster.length > 0 && (
          <RosterColumnHeads
            showBadge={isLive || isClosed}
            showReport={isLive || isClosed}
            showPrecedence={isLive || isClosed}
            heardAbsolute={isClosed}
          />
        )}
        <ul style={listStyle} aria-label="Roster" aria-live="polite">
          {unworkedRows.map(renderRosterRow)}
          {/* The worked block collapses to a count rather than
              dimming further — 0.55 on the muted token is already near the AA
              floor, so "make it dimmer" is unavailable. */}
          {workedRows.length > 0 && (
            <WorkedGroup count={workedRows.length}>
              {workedRows.map(renderRosterRow)}
            </WorkedGroup>
          )}
        </ul>
      </ResponsiveList>

      {/* The check-in detail modal — one level deep, live only. */}
      {isLive && editingEntry !== null && (
        <CheckInDetailModal
          sessionId={sessionId}
          entry={editingEntry}
          store={store}
          mode={netMode}
          viewerRole={viewerRole}
          onClose={() => setEditingKey(null)}
        />
      )}

      {/* Frequency / Start / Close are RunSession acts (NCS/Owner). A Relay
          holds none of them, so the whole control block is hidden — the server
          403s every one regardless. */}
      {/* While stalled, a ClaimControl-holder (Owner/NCS/
          Logger) sees the "Take control" rescue affordance. There is NO manual
          "resume" button — resume is presence-driven. */}
      {isLive && stalled && canClaimControl && (
        <div style={{ marginTop: "var(--space-4)" }}>
          <ClaimControl
            sessionId={sessionId}
            onClaimed={(summary) => store.getState().seedFromSnapshot(summary)}
          />
        </div>
      )}

      {/* On a HEALTHY (active) net, the current active NCS
          may voluntarily hand off to a qualified NetControl-tier target without
          interrupting the stream. Hidden while stalled (that's the claim path
          above) or when no eligible target is known. */}
      {isLive && !stalled && isActiveNcs && canRunSession && handoffTargets.length > 0 && (
        <div style={{ marginTop: "var(--space-4)" }}>
          <HandoffControl
            sessionId={sessionId}
            targets={handoffTargets}
            onHandedOff={(summary) => store.getState().seedFromSnapshot(summary)}
          />
        </div>
      )}
      {handoffTargetsProblem !== null && (
        <p role="alert" style={{ color: "var(--sync-text)", marginTop: "var(--space-2)" }}>
          {messageForProblem(handoffTargetsProblem)}
        </p>
      )}

      {canRunSession && (
        <div style={{ marginTop: "var(--space-4)" }}>
          {isLive ? (
            <CloseNetControl
              sessionId={sessionId}
              onClosed={(summary) => store.getState().seedFromSnapshot(summary)}
            />
          ) : (
            <StartNetControl definitionId={session.definitionId ?? ""} />
          )}
        </div>
      )}
      </div>
      </Panel>
    </main>
  );
}
