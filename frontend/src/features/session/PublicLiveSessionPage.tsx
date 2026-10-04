// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useMemo, useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useParams } from "react-router";
import { useStore } from "zustand";

import { useCurrentAccount } from "../auth/useCurrentAccount";
import { Breadcrumb } from "../../ui/components/Breadcrumb";
import { Panel } from "../../ui/components/Panel";
import { formatElapsedHms } from "../../ui/util/humanizeTime";
import { ResponsiveList } from "../../ui/layout/ResponsiveList";
import { ConnectionStatus } from "./ConnectionStatus";
import { ReplayingState } from "./ReplayingState";
import { RosterColumnHeads } from "./RosterColumnHeads";
import { RosterEntry } from "./RosterEntry";
import { SelfCheckInControl } from "./SelfCheckInControl";
import type { OwnCheckIn } from "./SelfCheckInControl";
import { getPublicEventsSince, getPublicSession } from "./sessionApi";
import { buildRoster, displayedConnection, selectYourTurnCheckInId } from "./sessionStore";
import { useSessionStream } from "./useSessionStream";
import type { SessionEndpoints } from "./useSessionStream";
import { tokens } from "../../ui/tokens/tokens";
import {
  rfConnection,
  sessionWaysIn,
} from "../nets/connectionPresentation";

/**
 * The PUBLIC, account-less live-session view (route `/live/:id`).
 * A directly-shared link renders this READ-ONLY page: the connection status
 * pill, the recovery banner, the operating-frequency pill, and the live roster —
 * with ZERO write affordances (no Start/Close/Frequency/Add-check-in controls)
 * and NO sign-in gate. It reuses the shipped reducer/store/socket/`RosterEntry`
 * wholesale, only pointed at the REDACTED account-less endpoints — so the
 * anonymous visitor folds the exact same event stream the owner does, minus the
 * operator/internal ids the public surface strips.
 *
 * A 404 (the session does not exist, or its id was never learnable) renders a
 * neutral not-found state — NEVER a redirect to `/sign-in`, because this view is
 * for a visitor with no account by design.
 */

/** The account-less redacted endpoint set — a module constant (stable ref). */
const PUBLIC_ENDPOINTS: SessionEndpoints = {
  fetchSnapshot: getPublicSession,
  fetchEventsSince: getPublicEventsSince,
  wsBasePath: (id) => `/api/net-sessions/${id}/live/ws`,
};

const pageStyle: CSSProperties = {
  // The content measure the shell chrome shares (DESIGN.md § Layout).
  maxWidth: "1200px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
};

/** The session-header band (design handoff `2a`): eyebrow + title + freq pill
 * on the left, the Live badge + Elapsed/Checked-in stat pair right-aligned.
 * Flush against the panel frame on the tonal header gradient. */
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

const listStyle: CSSProperties = { listStyle: "none", padding: 0, margin: 0 };

const notFoundStyle: CSSProperties = { color: "var(--text-muted)" };

/** The participant panel: the header band and the self-check-in footer bar run
 * flush to its edges, with the roster body padded between them. */
const panelStyle: CSSProperties = { margin: "var(--space-4) 0" };

const panelBodyStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-row-x)",
};

/** The self-check-in footer bar (design handoff `2a`): an info icon + a line
 * naming the viewer's own callsign, with the Staying/Check-out control
 * pushed to the far right — wrapping `SelfCheckInControl` (which owns all of
 * the check-in/toggle/check-out logic) in the mock's footer-bar chrome. */
const selfCheckInBarStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  padding: "var(--space-4) var(--space-row-x)",
  background: "var(--surface-2)",
  borderTop: "1px solid var(--border)",
  flexWrap: "wrap",
};

const selfCheckInTextStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
};

/**
 * The public live-session page. A thin param-reading shell, keyed on the route's
 * `id` so a param-only navigation fully remounts the inner store/stream (the
 * same reset posture as the owner page).
 */
export function PublicLiveSessionPage(): ReactElement {
  const { id } = useParams();
  return <PublicLiveSessionForSession key={id ?? ""} sessionId={id ?? ""} />;
}

interface PublicLiveSessionForSessionProps {
  readonly sessionId: string;
}

/** The actual public page body, mounted fresh per `sessionId` by its `key`ed parent. */
function PublicLiveSessionForSession({
  sessionId,
}: PublicLiveSessionForSessionProps): ReactElement {
  const { store, phase, resync } = useSessionStream(sessionId, PUBLIC_ENDPOINTS);
  // A SIGNED-IN participant lands on this same public view (route /live/:id):
  // their account unlocks the self-check-in control + the YourTurnIndicator. An
  // account-less viewer resolves to `null` and stays strictly read-only.
  const { account } = useCurrentAccount();

  const transport = useStore(store, (s) => s.connection);
  const session = useStore(store, (s) => s.session);
  const meta = useStore(store, (s) => s.meta);
  const pending = useStore(store, (s) => s.pending);
  // Memoize on the stable roster/pending/cursor refs — a selector returning a
  // fresh array would loop useSyncExternalStore. buildRoster is now called WITH
  // the worked-station cursor so the public roster renders the
  // coral working treatment.
  const roster = useMemo(
    () =>
      buildRoster(
        session.roster,
        pending,
        undefined,
        undefined,
        undefined,
        session.workingCheckInId,
        // The public page resolves the label the same way the
        // console does, against the same `connections` the public snapshot
        // already carries.
        session.connections,
      ),
    [session.roster, pending, session.workingCheckInId, session.connections],
  );
  // An account-less viewer sees net-paused too — the stalled
  // control status is public radio data, derived from the folded controlState.
  const connection = displayedConnection(transport, session.controlState);
  // The self-check-in write is a
  // roster mutation, so it freezes while stalled exactly like LiveSessionPage's
  // FrequencyControl/QuickAddRow — UX only, the server 409s `session-paused`
  // regardless.
  const stalled = session.controlState === "stalled";

  const ownCallsign = account?.callsign ?? null;
  // The viewer's own next-up row — a pure selector over the display-order
  // roster + cursor + own callsign. `null` unless the viewer's row is next up.
  // The public snapshot and delta now carry the ordering mode, and
  // the selector needs it — under worked-sink "next up" is the top of the
  // unworked group, which can sit ABOVE the cursor.
  const yourTurnCheckInId = selectYourTurnCheckInId(
    session.roster,
    session.workingCheckInId,
    ownCallsign,
    session.rosterOrderMode,
  );
  // The viewer's OWN folded entry (callsign match), for the self toggle/checkout.
  const ownEntry =
    ownCallsign === null
      ? undefined
      : session.roster.find((e) => e.callsign.toUpperCase() === ownCallsign.toUpperCase());
  const ownCheckIn: OwnCheckIn | null =
    ownEntry === undefined
      ? null
      : { checkInId: ownEntry.checkInId, staying: ownEntry.staying, version: ownEntry.version };

  // Ticks the session-header "Elapsed" stat once a second while live.
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const tick = window.setInterval(() => setNow(new Date()), 1000);
    return () => window.clearInterval(tick);
  }, []);

  if (phase.status === "loading") {
    return <main style={pageStyle} />;
  }

  // `signed-out` here means the account-less snapshot resolved to null (a 404):
  // the session does not exist, or its id was never learnable. Render a neutral
  // not-found state — never a sign-in redirect (this view is account-less).
  if (phase.status === "signed-out" || phase.status === "error") {
    return (
      <main style={pageStyle}>
        <p role="status" style={notFoundStyle}>
          This session isn&rsquo;t available.
        </p>
      </main>
    );
  }

  const definitionTitle = meta?.definition.title ?? "Live session";
  // See `LiveSessionPage`: band/mode live on the connection set
  // now, and the one connection presenter renders it.
  const wayIn = sessionWaysIn(session.connections);
  const rfWay = rfConnection(session.connections);
  const bandCategoryLabel =
    meta === null
      ? "Nets"
      : `${rfWay?.band ?? "Internet"} · ${meta.definition.netCategory}`;

  return (
    <main style={pageStyle}>
      <Breadcrumb
        items={[
          { label: "Nets", href: "/" },
          { label: bandCategoryLabel },
          { label: definitionTitle },
        ]}
      />
      <Panel style={panelStyle}>
        <div style={sessionHeaderStyle} data-session-header>
        <div>
          <div style={eyebrowStyle}>
            {session.lifecycle === "live" ? "Live net · you're watching" : "Net session"}
          </div>
          <h2 style={titleStyle}>{definitionTitle}</h2>
          {meta !== null && (
            <div style={freqStyle}>
              <span className="mono" style={{ fontSize: tokens.typography.callsign.fontSize, fontWeight: 800 }}>
                {wayIn}
              </span>
            </div>
          )}
        </div>
        <div style={headerRightStyle}>
          <ConnectionStatus connection={connection} />
          <div style={statPairStyle}>
            {session.startedAt !== null && (
              <span>
                Elapsed{" "}
                <b className="mono" style={{ color: "var(--text)" }}>
                  {formatElapsedHms(session.startedAt, now)}
                </b>
              </span>
            )}
            <span>
              Checked in <b style={{ color: "var(--text)" }}>{session.roster.length}</b>
            </span>
          </div>
        </div>
      </div>

      <div style={panelBodyStyle}>
      {/* net-paused is a control render, not a transport resync trigger. */}
      <ReplayingState connection={transport} onResync={resync} />

      {/* Wrapped in ResponsiveList: wide roster content scrolls
          inside this container on a narrow desktop window, never the page
          body. */}
      <ResponsiveList>
        {/* Observer density: position + station + source +
            precedence + heard. The head strip mirrors the row flags below it, so
            adding the precedence cell to the rows adds its label here in the same
            change. The REPORT column is still the operator console's —
            it is not one of the four fields that moved. */}
        {roster.length > 0 && <RosterColumnHeads showPosition showBadge showPrecedence />}
        <ul style={listStyle} aria-label="Roster">
          {roster.map((entry, index) => (
            <RosterEntry
              key={entry.key}
              entry={entry}
              showSourceBadge
              showWorked
              // Staying, precedence and the traffic count
              // reach EVERY observer — this page renders identically whether
              // `account` resolved to a signed-in participant or to `null`, and
              // nothing here branches on it. The account-less viewer is the
              // intended reach, not an edge case to hedge against.
              showStaying
              showPrecedence
              position={index + 1}
              yourTurn={entry.key === yourTurnCheckInId}
            />
          ))}
        </ul>
      </ResponsiveList>
      </div>

      {/* The self-check-in footer bar (design handoff `2a`) — renders for EVERY
          viewer, signed-in or not: `SelfCheckInControl`'s own
          `selfCheckInGate` handles a `null` account by routing to `/sign-in`
          with `returnTo` — an account-less viewer taps "Check in" and is
          routed to sign in and returned to this action, never gaining a write
          directly (no write affordance actually SUCCEEDS for
          an account-less viewer, since the gate intercepts the click before
          any API call is made). Gating this behind `account !== null` would
          make that signed-out routing branch unreachable in production.
          Hidden while stalled — a roster mutation the server refuses
          regardless. */}
      {!stalled && (
        <div style={selfCheckInBarStyle}>
          {ownCheckIn !== null && (
            <span style={selfCheckInTextStyle}>
              You&rsquo;re checked in as{" "}
              <span className="mono" style={{ color: "var(--text)", fontWeight: 800 }}>
                {ownCallsign}
              </span>
              . Toggle staying, or check out anytime.
            </span>
          )}
          <div style={{ marginLeft: "auto" }}>
            <SelfCheckInControl
              sessionId={sessionId}
              store={store}
              account={account}
              ownCheckIn={ownCheckIn}
            />
          </div>
        </div>
      )}
      </Panel>
    </main>
  );
}
