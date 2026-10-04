// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useRef, useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useParams } from "react-router";

import { messageForProblem } from "../../errors/problemMessages";
import { Breadcrumb } from "../../ui/components/Breadcrumb";
import { FavoriteToggle } from "../../ui/components/FavoriteToggle";
import { Panel } from "../../ui/components/Panel";
import { tokens } from "../../ui/tokens/tokens";
import { useAuthRequest } from "../auth/useAuthRequest";
import { useCurrentAccount } from "../auth/useCurrentAccount";
import {
  describeConnections,
  rfConnection,
  type ConnectionDisplay,
} from "./connectionPresentation";
import { favoriteNet, fetchFavoriteMembership, unfavoriteNet } from "./favoritesApi";
import {
  formatFrequencyMhz,
  getNetByToken,
  type PublicNetView,
} from "./netsApi";

const pageStyle: CSSProperties = {
  maxWidth: "760px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
  fontSize: tokens.typography.body.fontSize,
  lineHeight: tokens.typography.body.lineHeight,
};

/** The header band: title + favorite star over the tonal gradient, flush to
 * the panel frame that clips it. */
const headerBandStyle: CSSProperties = {
  display: "flex",
  alignItems: "flex-start",
  justifyContent: "space-between",
  gap: "var(--space-3)",
  flexWrap: "wrap",
  padding: "var(--space-5) var(--space-row-x)",
  background: "var(--head-grad)",
  borderBottom: "1px solid var(--border)",
};

const headingStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  lineHeight: tokens.typography.sessionTitle.lineHeight,
  margin: 0,
};

const freqPillStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
  marginTop: "var(--space-3)",
  background: "var(--freq-fill)",
  border: "1px solid var(--freq-border)",
  color: "var(--freq-text)",
  borderRadius: "var(--rounded-freq)",
  padding: "var(--space-2) var(--space-3)",
  fontSize: tokens.typography.body.fontSize,
};

const freqValueStyle: CSSProperties = {
  fontSize: tokens.typography.callsign.fontSize,
  fontWeight: 800,
};

/** The amber archived banner: fill + border + icon + label, so the state never
 * rests on colored text alone. */
const archivedBannerStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  padding: "var(--space-3) var(--space-row-x)",
  background: "var(--catch-fill)",
  color: "var(--catch-text)",
  borderBottom: "1px solid var(--catch-border)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
};

const specStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
};

/** One label/value row. The shared grid template is what lets every value line
 * up in a column instead of trailing its own label. */
const specRowStyle: CSSProperties = {
  display: "grid",
  gridTemplateColumns: "minmax(120px, 160px) 1fr",
  gap: "var(--space-3)",
  alignItems: "baseline",
  padding: "var(--space-3) var(--space-row-x)",
  borderBottom: "1px solid var(--border)",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
};

// `pre-line` is what preserves the author's paragraph breaks on this page, and
// it is deliberate rather than defaulted — do not "improve" it.
//
// What `pre-line` does NOT do. It does not collapse runs
// of blank lines. Measured in headless Firefox, "A" + "\n"×N + "B" renders at
// an identical height under `pre-line` and under `pre-wrap` (396px each, vs
// 22px under `normal`); CSS Text 3 §4.1.2 makes segment breaks non-collapsible
// under `pre`, `pre-wrap`, `break-spaces` AND `pre-line` — only spaces and tabs
// differ between the two values. An earlier version of this comment asserted
// the collapse as fact, and named a harm ("pushes the frequency, schedule and
// how-to-join off screen") that cannot occur under any value, because this <p>
// is the Panel's last child and nothing follows it to push.
//
// That left a live hazard: MAX_DESCRIPTION_CHARS bounds the field at 2000
// characters AFTER newline normalisation, so "a" + "\n"×1998 + "b" was a legal,
// storable description that rendered ~1998 empty line boxes on a page anyone
// with the link token can reach.
//
// Closed at the parse layer and NOT here:
// `parse_bounded_multiline_text` collapses a run of three or more consecutive
// newlines to two, after CRLF normalisation and before the length bound. This
// page can therefore no longer be handed a description containing such a run,
// so no render-side mitigation belongs here and `pre-line` stays. Two residues
// are known and not fixed here: newlines separated by spaces are not
// "consecutive" and survive the collapse, and a description stored before the
// collapse landed is not re-parsed on read, so it keeps its run until the net
// is next saved.
//
// `anywhere` is what stops the opposite input — one unbroken 2000-character run
// with no whitespace — from widening the panel past the viewport.
const descriptionStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-row-x)",
  margin: 0,
  whiteSpace: "pre-line",
  overflowWrap: "anywhere",
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
};

/** Joins the present geography parts into one line, or `null` when empty. */
function geographyLine(net: PublicNetView): string | null {
  const parts = [net.state, net.country, net.grid].filter(
    (part): part is string => part !== null && part !== "",
  );
  return parts.length > 0 ? parts.join(" · ") : null;
}

/**
 * The access facts a visitor needs to actually join, taken from the net's
 * CONNECTION SET rather than from the flat mirror columns.
 *
 * The mirror goes stale by design: a net with no RF connection leaves
 * `plannedFrequencyHz`, `band` and `mode` holding whatever they last did, and
 * this page is where that would be published to the world as fact. Every
 * connection is rendered, in the owner's order, with only the properties its
 * own kind carries.
 */
function accessRows(net: PublicNetView): readonly (readonly [string, string])[] {
  const displays = describeConnections(net.connections);
  // The pill above already carries this connection's frequency, band and mode,
  // so repeating them here is the duplication the spec layout was built to
  // avoid — every other fact the pill has no room for still belongs.
  const pillId = rfConnection(net.connections)?.id ?? null;
  const seen = new Map<string, number>();
  return displays.flatMap((display) => {
    seen.set(display.kindLabel, (seen.get(display.kindLabel) ?? 0) + 1);
    const prefix = rowPrefix(displays, display, seen.get(display.kindLabel) ?? 1);
    return display.facts
      .filter(([label]) => !(display.id === pillId && label === "Frequency"))
      .map(([label, value]) => [`${prefix}${label}`, value] as const);
  });
}

/** What to put in front of a fact's label so a reader knows which connection
 * it belongs to. Nothing for a net with one way to reach it — the old
 * single-connection reading, unchanged — the kind for a net with several, and
 * a number as well when two share a kind. */
function rowPrefix(
  displays: readonly ConnectionDisplay[],
  display: ConnectionDisplay,
  ordinal: number,
): string {
  if (displays.length === 1) {
    return "";
  }
  const shared = displays.filter(
    (other) => other.kindLabel === display.kindLabel,
  ).length;
  return `${display.kindLabel}${shared > 1 ? ` ${ordinal}` : ""} · `;
}

/**
 * The header pill. It describes the net's first RF connection; a net reached
 * only over the internet has NO frequency and NO band, and is named by its
 * connections instead — inventing one from the stale flat mirror is the thing
 * this page was publishing as fact.
 */
function FrequencyPill({ net }: { net: PublicNetView }): ReactElement {
  const rf = rfConnection(net.connections);
  if (rf === null || rf.plannedFrequencyHz === null) {
    const named = describeConnections(net.connections)
      .map((display) => {
        const first = display.facts[0];
        return first === undefined
          ? display.kindLabel
          : `${display.kindLabel} ${first[1]}`;
      })
      .join(" · ");
    return (
      <span data-testid="freq-pill" style={freqPillStyle}>
        <span className="mono" style={freqValueStyle}>
          {named}
        </span>
      </span>
    );
  }
  return (
    <span data-testid="freq-pill" style={freqPillStyle}>
      <span className="mono" style={freqValueStyle}>
        {formatFrequencyMhz(rf.plannedFrequencyHz)} MHz
      </span>
      <span>
        {rf.band === null ? "" : ` · ${rf.band}`}
        {rf.mode === null ? "" : ` · ${rf.mode}`}
      </span>
    </span>
  );
}

/** A warning triangle for the archived banner. */
function ArchivedIcon(): ReactElement {
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
      <path d="M12 8v5M12 16h.01" />
      <path d="M10.3 3.9 2.6 18a2 2 0 0 0 1.7 3h15.4a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
    </svg>
  );
}

/** A label/value row in the spec list. `mono` marks radio data. */
function SpecRow({
  label,
  children,
}: {
  label: string;
  children: ReactElement | string;
}): ReactElement {
  return (
    <div data-spec-row style={specRowStyle}>
      <span style={labelStyle}>{label}</span>
      <span>{children}</span>
    </div>
  );
}

/**
 * Keeps a `noindex, nofollow` robots directive in the document head for as long
 * as a permalink page is mounted, and removes it on the way out.
 *
 * Discovery now links every Listed net's title to `/nets/t/{token}`, so a JS-executing
 * crawler can walk the public landing page straight into every permalink and
 * put the token in a third-party index. `find_by_link_token` applies no
 * visibility filter, so an indexed URL keeps resolving after the owner flips
 * the net to Unlisted — and there is no token rotation. The directive is
 * scoped to this route alone: nothing else on the site gains one.
 *
 * A `robots.txt` disallow was the alternative and does less — it asks politely
 * and leaves the URL indexable when someone links to it from elsewhere.
 *
 * THIS TAG IS THE SECOND LAYER, NOT THE ONLY ONE, and the distinction matters
 * because it is easy to read the paragraph above as though it were sufficient.
 * The directive exists only once React has mounted, so a crawler that fetches
 * the SPA shell without executing JS sees no directive at all. The layer that
 * holds without JS is `X-Robots-Tag: noindex, nofollow` on `/nets/t/*`, served
 * by the public Caddy on the routing host — outside this repo and outside CI,
 * so it cannot be asserted from here and is recorded instead. Do not delete
 * this hook on the
 * grounds that the header covers it: the header does not travel with the app,
 * and a deploy behind a different front door would lose it silently.
 */
function useNoIndex(): void {
  useEffect(() => {
    const meta = document.createElement("meta");
    meta.name = "robots";
    meta.content = "noindex, nofollow";
    document.head.appendChild(meta);
    return () => {
      meta.remove();
    };
  }, []);
}

/**
 * PUBLIC, read-only view of a net reached by its unguessable link token
 * It stays public — an unauthenticated visitor with the link
 * sees the net, with no consent/callsign gate and no redirect. A missing/wrong
 * token surfaces the mapped not-found message (the uniform 404), never a
 * distinct "forbidden" signal.
 *
 * The account IS fetched — but only to decide whether to offer the
 * favorite affordance: a signed-in visitor gets a favorite toggle (this is the
 * primary favorite-write surface, favoriting `net.id`); a signed-out one sees
 * none. A 401 from the account fetch simply reads as signed-out; it never
 * blocks or redirects the public read.
 */
export function PublicNetPage(): ReactElement {
  const { token } = useParams();
  const { account } = useCurrentAccount();
  const { state, run } = useAuthRequest<PublicNetView>(() =>
    getNetByToken(token ?? ""),
  );
  // Local, await-server-then-reflect favorite state. Starts unfavorited, then
  // corrected below once the account's actual favorites are known — the
  // toggle must never lie about state for a net the account already
  // favorited on a prior visit.
  const [favorited, setFavorited] = useState(false);
  // Moves each time the visitor's own toggle is confirmed. The membership read
  // below describes the server as it stood when the read BEGAN, so an answer
  // that lands after a toggle would put the star back the way it was; the
  // read checks the generation it started under and discards a stale answer.
  // `cancelled` cannot do this — it covers unmount and a changed dependency,
  // and a toggle is neither.
  const toggleGeneration = useRef(0);

  useNoIndex();

  useEffect(() => {
    void run();
  }, [run, token]);

  const netId = state.status === "success" ? state.data.id : null;

  useEffect(() => {
    if (account === null || netId === null) {
      setFavorited(false);
      return;
    }
    let cancelled = false;
    const generation = toggleGeneration.current;
    fetchFavoriteMembership([netId])
      .then((favoritedIds) => {
        if (!cancelled && toggleGeneration.current === generation) {
          setFavorited(favoritedIds.has(netId));
        }
      })
      .catch(() => {
        // Leave the toggle unfavorited; nothing to reconcile with.
      });
    return () => {
      cancelled = true;
    };
  }, [account, netId]);

  if (state.status === "error") {
    return (
      <main style={pageStyle}>
        <p role="alert" style={errorStyle}>
          {messageForProblem(state.problem)}
        </p>
      </main>
    );
  }

  if (state.status !== "success") {
    // idle | loading — render nothing rather than flash a title.
    return <main style={pageStyle} />;
  }

  const net = state.data;
  const geography = geographyLine(net);

  const onToggleFavorite = async (next: boolean): Promise<void> => {
    if (next) {
      await favoriteNet(net.id);
    } else {
      await unfavoriteNet(net.id);
    }
    // Reflect only after the server confirms the write — and outrank any
    // membership answer still in flight, which predates it.
    toggleGeneration.current += 1;
    setFavorited(next);
  };

  return (
    <main style={pageStyle}>
      {/* A link-token arrival often has no history to go back through, so the
          trail is the only route into the rest of the app. */}
      <Breadcrumb items={[{ label: "Nets", href: "/" }, { label: net.title }]} />

      <Panel style={{ marginTop: "var(--space-4)" }}>
        <div data-net-header style={headerBandStyle}>
          <div>
            <h1 style={headingStyle}>{net.title}</h1>
            <FrequencyPill net={net} />
          </div>
          {account !== null && (
            <FavoriteToggle favorited={favorited} onToggle={onToggleFavorite} />
          )}
        </div>

        {net.archivedAt !== null && (
          <div
            data-testid="archived-notice"
            role="status"
            style={archivedBannerStyle}
          >
            <ArchivedIcon />
            This net was archived and is no longer active.
          </div>
        )}

        {/* Band and mode are NOT repeated here — the freq pill above already
            carries them. These are the facts the pill has no room for. */}
        <div data-testid="net-spec" style={specStyle}>
          <SpecRow label="Category / Type">
            {`${net.netCategory} · ${net.netType}`}
          </SpecRow>
          {accessRows(net).map(([label, value]) => (
            <SpecRow key={label} label={label}>
              <span className="mono">{value}</span>
            </SpecRow>
          ))}
          {geography !== null && (
            <SpecRow label="Location">
              <span className="mono">{geography}</span>
            </SpecRow>
          )}
          {net.expectedDurationMinutes !== null && (
            <SpecRow label="Runs for">
              {`~${net.expectedDurationMinutes} min`}
            </SpecRow>
          )}
        </div>

        {net.description !== null && net.description !== "" && (
          <p data-testid="net-description" style={descriptionStyle}>
            {net.description}
          </p>
        )}
      </Panel>
    </main>
  );
}
