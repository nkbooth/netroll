// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useSearchParams } from "react-router";

import { messageForProblem } from "../../errors/problemMessages";
import type { Problem } from "../auth/authApi";
import { ProblemError } from "../auth/authApi";
import { useCurrentAccount } from "../auth/useCurrentAccount";
import { Breadcrumb } from "../../ui/components/Breadcrumb";
import { DescriptionPreview } from "../../ui/components/DescriptionPreview";
import { FavoriteToggle } from "../../ui/components/FavoriteToggle";
import { Logomark } from "../../ui/components/Logomark";
import { Panel, PanelColumnHeads } from "../../ui/components/Panel";
import { ResponsiveList, useLayout } from "../../ui/layout/ResponsiveList";
import { tokens } from "../../ui/tokens/tokens";
import {
  absoluteLocalTimeWithWeekday,
  humanizeTime,
} from "../../ui/util/humanizeTime";
import { BANDS, CATEGORIES, MODES, NET_TYPES } from "../nets/netEnums";
import { favoriteNet, fetchFavoriteMembership, unfavoriteNet } from "../nets/favoritesApi";
import {
  CONNECTION_KINDS,
  KIND_LABELS,
  connectionSummary,
  connectionSummaryLines,
} from "../nets/connectionPresentation";
import { netPermalink } from "../nets/netPermalink";
import {
  FILTER_KEYS,
  discoverySearchParams,
  filtersFromSearchParams,
  getDiscovery,
} from "./discoveryApi";
import type {
  DiscoveryFilters,
  DiscoveryNet,
  DiscoveryResponse,
} from "./discoveryApi";

/** The sort keys that still have a defensible answer. `band`
 * and `mode` left when a net gained a SET of connections: there is no ORDER BY
 * over a set. A URL still carrying one is the server's to answer — it falls back
 * and says so — so this list is deliberately NOT used to scrub the URL. */
const SORTS = ["time", "name", "category", "type"];

/** Delay (ms) between the last filter change and the re-fetch it triggers.
 * The discovery endpoint is PUBLIC, carries no rate limiter, and its free-text
 * `q` filter is an unindexed SQL substring scan — firing a request per
 * keystroke would needlessly multiply that scan's cost for every visitor
 * typing a search term. The initial mount load is NOT debounced: the page
 * must start loading immediately. */
const FILTER_DEBOUNCE_MS = 300;

/** The upcoming panel's row grid, shared by the head strip and every row so
 * the columns actually line up: #, net, freq, when, favorite. */
// The ways-in track is `minmax(0, 150px)` rather than a bare `150px` as
// INSURANCE and is honestly labelled as such: measured in chromium it made no
// difference, because a fixed track already cannot grow.
// It costs nothing and states the intent. What actually contains the cell is
// in `freqPillStyle`.
const UPCOMING_TEMPLATE = "26px minmax(0, 1.7fr) minmax(0, 150px) 92px 40px";

/** Filters kept in the toolbar itself — the hot path a visitor reaches for
 * first. Everything else lives behind the disclosure. `kind` is here because the
 * gap it closes — an internet-only net findable by neither band nor mode — is
 * invisible to a visitor who never opens the disclosure. A
 * chip rendered in the toolbar whose key is NOT in this list makes the
 * "More filters" badge count a visible filter as hidden. */
const PRIMARY_FILTER_KEYS = ["q", "band", "mode", "kind"] as const;

/** The echo's render order is `FILTER_KEYS` — the same list that serialises the
 * URL and parses it back. Fixed rather than derived from `Object.keys`, so the
 * statement does not reorder itself between responses; shared rather than
 * duplicated, so a new filter cannot be added to one and forgotten in the other.
 * `sort` is last because it is always present. */
const APPLIED_LABELS: Record<(typeof FILTER_KEYS)[number], string> = {
  q: "Search",
  band: "Band",
  mode: "Mode",
  // "Connection", not "Connection kind": the list reads `Band: 20m`, and
  // `Connection: EchoLink` is English where `Connection kind: EchoLink` is not.
  // The control keeps the disambiguating word because it needs one.
  kind: "Connection",
  country: "Country",
  state: "State",
  grid: "Grid",
  category: "Category",
  type: "Net type",
  sort: "Sorted by",
};

/** The echo key carrying the sort the server could NOT honour.
 *
 * Deliberately absent from `APPLIED_LABELS`: that map labels the "Showing" list,
 * whose accessible name promises the filters and sort the server APPLIED, and
 * this one was not applied. It gets its own statement instead. */
const SORT_UNAVAILABLE_KEY = "sortUnavailable";

/** The echo key naming the collections the server CUT.
 *
 * Likewise absent from `APPLIED_LABELS` and, like `sortUnavailable`, excluded
 * from the applied list BY NAME rather than by shape: today its value is an
 * array, which the list's `typeof value === "string"` guard happens to drop,
 * and relying on that would let the next shape change render
 * `truncated: activeNow` under an accessible name promising applied filters —
 * a statement the server never made. It gets its own statement, one per
 * collection, beside the collection it describes. */
const TRUNCATED_KEY = "truncated";

/** The wire names the server uses for the two collections in `truncated`. */
const ACTIVE_NOW_COLLECTION = "activeNow";
const UPCOMING_COLLECTION = "upcoming";

/** A dimension the server applied that this client has never heard of still has
 * to be stated; it is labelled by its own key rather than dropped, because a
 * short client-side literal is not a reason to hide what the server did.
 *
 * `Object.hasOwn`, never `key in` — the key comes off unvalidated network JSON,
 * and `in` walks the prototype chain, so a response echoing `__proto__` or
 * `toString` would resolve the lookup to `Object.prototype` or a function and
 * hand THAT to React as a child, which is the render crash this bar was fixed
 * to stop. `JSON.parse` makes `__proto__` an ordinary own enumerable key, so it
 * reaches this function like any other. */
function appliedLabel(key: string): string {
  return Object.hasOwn(APPLIED_LABELS, key)
    ? APPLIED_LABELS[key as (typeof FILTER_KEYS)[number]]
    : key;
}

type LoadState =
  | { status: "loading" }
  | { status: "success"; data: DiscoveryResponse }
  | { status: "error"; problem?: Problem };

const pageStyle: CSSProperties = {
  maxWidth: "1200px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-5)",
  fontSize: tokens.typography.body.fontSize,
  lineHeight: tokens.typography.body.lineHeight,
};

// ── Hero identity band ──────────────────────────────────────────────────────

const heroStyle: CSSProperties = {
  display: "flex",
  alignItems: "flex-start",
  gap: "var(--space-4)",
  padding: "var(--space-5) var(--space-row-x)",
  background: "var(--head-grad)",
  border: "1px solid var(--border)",
  // The single cyan signature, as a left edge rather than a filled hero block.
  borderLeft: "var(--space-status-bar) solid var(--accent)",
  borderRadius: "var(--rounded-xl)",
  boxShadow: "var(--shadow)",
};

const kickerStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--accent-ink)",
};

const pitchStyle: CSSProperties = {
  margin: "var(--space-1) 0 0",
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  lineHeight: tokens.typography.sessionTitle.lineHeight,
};

const statusStripStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  marginTop: "var(--space-3)",
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
  flexWrap: "wrap",
};

const statusCountStyle: CSSProperties = {
  color: "var(--text)",
  fontWeight: 700,
};

const pulseDotStyle: CSSProperties = {
  width: "9px",
  height: "9px",
  borderRadius: "var(--rounded-full)",
  background: "var(--live-dot)",
  boxShadow: "0 0 8px var(--live-dot)",
  flex: "0 0 auto",
};

// ── Shared bits ─────────────────────────────────────────────────────────────

const monoStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  fontVariantNumeric: "tabular-nums",
};

// `whiteSpace: "nowrap"` used to close this object. Dropping it plus
// `overflowWrap: "anywhere"` are the TWO moves measured in real chromium to
// contain the cell: on a hostile fixture carrying one unbreakable
// 26-character reflector token the pill's scrollWidth goes 228 -> 148, and on
// an ordinary four-connection net 689 -> 148.
//
// `MyNetsPage`'s favorites row is the SUPPORTING precedent, not a
// counter-example — it names the exact mechanism: `overflow-wrap` lowers
// MIN-content. There that was the wrong lever, because a flex item's basis is
// measured at MAX-content. Here min-content is precisely the binding
// constraint: an unbreakable word's min-content is what pins a grid item wider
// than a track that cannot grow.
//
// `minWidth` / `maxWidth` are INSURANCE and had no measurable effect in either
// probe. They are kept for consistency with the sibling Net cell — the pill IS
// the grid item at the Upcoming row — and are not the fix.
const freqPillStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
  background: "var(--freq-fill)",
  border: "1px solid var(--freq-border)",
  color: "var(--freq-text)",
  borderRadius: "var(--rounded-freq)",
  padding: "var(--space-1) var(--space-3)",
  fontSize: tokens.typography.meta.fontSize,
  overflowWrap: "anywhere",
  minWidth: 0,
  maxWidth: "100%",
};

const freqValueStyle: CSSProperties = {
  ...monoStyle,
  fontWeight: 800,
};

const liveBadgeStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
  borderRadius: "var(--rounded-full)",
  padding: "var(--space-1) var(--space-3)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 800,
  border: "1px solid var(--live-border)",
  background: "var(--live-fill)",
  color: "var(--live-text)",
  whiteSpace: "nowrap",
};

const metaStyle: CSSProperties = {
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
};

const bodyPadStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-row-x)",
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
};

const secondaryButtonStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-3)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
  marginTop: "var(--space-3)",
};

// ── Live (on-air) panel ─────────────────────────────────────────────────────

const featuredStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-row-x)",
  borderLeft: "var(--space-status-bar) solid var(--accent)",
  background: "var(--self-fill)",
};

const featuredHeaderStyle: CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  alignItems: "flex-start",
  gap: "var(--space-3)",
  flexWrap: "wrap",
};

const featuredTitleStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  margin: 0,
};

const featuredActionsStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  marginTop: "var(--space-4)",
  flexWrap: "wrap",
};

const watchLiveLinkStyle: CSSProperties = {
  background: "var(--accent-deep)",
  border: "1px solid var(--accent-deep)",
  color: "var(--on-accent)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-4)",
  fontWeight: 800,
  fontSize: tokens.typography.meta.fontSize,
  textDecoration: "none",
};

const checkInLinkStyle: CSSProperties = {
  background: "transparent",
  border: "1px solid var(--border)",
  color: "var(--text)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-3)",
  fontWeight: 700,
  fontSize: tokens.typography.meta.fontSize,
  textDecoration: "none",
};

const compactRowStyle: CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  alignItems: "center",
  gap: "var(--space-3)",
  padding: "var(--space-3) var(--space-row-x)",
  borderTop: "1px solid var(--border)",
  flexWrap: "wrap",
};

const nextUpStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  marginTop: "var(--space-3)",
  flexWrap: "wrap",
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
};

// ── Filter toolbar ──────────────────────────────────────────────────────────

const toolbarStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  padding: "var(--space-3) var(--space-row-x)",
  background: "var(--surface-2)",
  borderBottom: "1px solid var(--border)",
  flexWrap: "wrap",
};

/** The neutral chip: a filter not currently narrowing the results. */
const chipStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-3)",
  background: "var(--surface)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
  font: "inherit",
};

/** The accent-filled chip: this filter is currently set. */
const chipActiveStyle: CSSProperties = {
  ...chipStyle,
  background: "var(--accent-deep)",
  border: "1px solid var(--accent-deep)",
  color: "var(--on-accent)",
};

const disclosureStyle: CSSProperties = {
  ...chipStyle,
  marginLeft: "auto",
  cursor: "pointer",
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
};

const activeCountStyle: CSSProperties = {
  ...monoStyle,
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  borderRadius: "var(--rounded-sm)",
  padding: "0 var(--space-1)",
  fontSize: tokens.typography.microCaps.fontSize,
  fontWeight: 800,
};

const appliedBarStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  padding: "var(--space-2) var(--space-row-x)",
  borderBottom: "1px solid var(--border)",
  flexWrap: "wrap",
};

const appliedListStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  listStyle: "none",
  margin: 0,
  padding: 0,
  flexWrap: "wrap",
};

const appliedItemStyle: CSSProperties = {
  ...metaStyle,
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-sm)",
  padding: "0 var(--space-2)",
};

const secondaryFiltersStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  padding: "var(--space-3) var(--space-row-x)",
  background: "var(--surface-2)",
  borderBottom: "1px solid var(--border)",
  flexWrap: "wrap",
};

// ── Upcoming rows ───────────────────────────────────────────────────────────

const rowBaseStyle: CSSProperties = {
  gap: "var(--space-gutter)",
  padding: "var(--space-row-y) var(--space-row-x)",
  borderBottom: "1px solid var(--border)",
};

const rowGridStyle: CSSProperties = {
  ...rowBaseStyle,
  display: "grid",
  gridTemplateColumns: UPCOMING_TEMPLATE,
  alignItems: "center",
};

const rowStackedStyle: CSSProperties = {
  ...rowBaseStyle,
  display: "flex",
  flexDirection: "column",
  alignItems: "flex-start",
};

const rowPosStyle: CSSProperties = {
  ...monoStyle,
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
};

const rowTitleStyle: CSSProperties = {
  fontWeight: 700,
};

/** The title-as-link hook. The treatment itself — resting colour
 * and no underline, plus the `accent-ink` + underline hover — is ENTIRELY in
 * `index.css` under this class, and deliberately not an inline style object:
 * an inline declaration outranks any author stylesheet, so a resting look set
 * inline makes the `:hover` rule unreachable, which is how it once shipped.
 * Inline styles here carry layout only. */
const NET_TITLE_LINK_CLASS = "net-title-link";

const rowTimeStyle: CSSProperties = {
  ...monoStyle,
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
};

/** The absolute local time that accompanies the relative phrase, on its own
 * line so the narrow/stacked row layout does not have to
 * widen for it. `<time>` is a grid/flex item at every render site, so a block
 * child lays out normally. */
const rowTimeAbsoluteStyle: CSSProperties = {
  display: "block",
  color: "var(--text-muted)",
  opacity: 0.85,
};

/** Marks the connection the SERVER matched. Distinguished
 * accessibly as well as visually: axe has no rule for two elements sharing an
 * accessible name, so a green scan is not evidence that "which one matched" is
 * answerable by a screen reader. */
const matchedConnectionStyle: CSSProperties = {
  ...monoStyle,
  color: "var(--text)",
  background: "var(--surface-2)",
  border: "1px solid var(--warn)",
  borderRadius: "var(--rounded-sm)",
  padding: "0 var(--space-1)",
  fontSize: tokens.typography.microCaps.fontSize,
};

/* How many ways in the ONE cell that sits in a fixed track will show.
 *
 * Only one way in fits the screen width at the original row height; more is
 * only safe if the row grows. That condition is met, and by measurement rather
 * than argument: with `nowrap` off, the four-connection row in real chromium
 * is 108.6px against the one-line row's 40px. The row does widen, so three is
 * free. Module-private — nothing outside this file has a reason to know the
 * number, and the test asserts the cap through the rendered cell. */
const MAX_CELL_CONNECTIONS = 3;

/* A column at the capped site so the `match:` mark stacks UNDER the summary
 * instead of beside it. Two side-by-side blocks in 150px is the overrun in
 * miniature, and the cap bounds only the summary. */
const boundedPillStyle: CSSProperties = {
  ...freqPillStyle,
  flexDirection: "column",
  alignItems: "flex-start",
};

const connectionLineStyle: CSSProperties = {
  display: "block",
};

const overflowLinkStyle: CSSProperties = {
  color: "var(--freq-text)",
  fontSize: tokens.typography.microCaps.fontSize,
};

/** The connection pill: what the net's OWN connections say.
 *
 * It used to render `plannedFrequencyHz · band · mode` straight off the flat
 * mirror columns. Those go stale the moment a net drops its last RF way — the
 * writer will not invent a value for a NOT NULL column it has none for — so an
 * EchoLink-only net's card published `14.230 MHz · 20m · ssb` to the world as
 * fact. `connectionSummary` is the SHIPPED presenter `MyNetsPage` already
 * uses; a fourth hand-written one here is how they drift.
 *
 * When the server matched a band or mode filter it names the connection it
 * matched on, and that connection is marked. Never re-derived from the set: a
 * client-side twin of the filter predicate that drifts marks a connection the
 * server did NOT match on, which is a confident wrong answer to the one
 * question the mark exists to answer. */
function ConnectionPill({
  item,
  bounded = false,
}: {
  item: DiscoveryNet;
  bounded?: boolean;
}): ReactElement {
  const matched =
    item.matchedConnectionId === null
      ? undefined
      : item.connections.find((c) => c.id === item.matchedConnectionId);
  const lines = connectionSummaryLines(item.connections);
  const shown = bounded ? lines.slice(0, MAX_CELL_CONNECTIONS) : lines;
  const remainder = lines.length - shown.length;
  return (
    <span
      data-testid="connection-cell"
      style={bounded ? boundedPillStyle : freqPillStyle}
    >
      <span data-testid="connection-summary" style={freqValueStyle}>
        {bounded
          ? shown.map((line, index) => (
              <span
                key={`${String(index)}:${line}`}
                data-testid="connection-line"
                style={connectionLineStyle}
              >
                {line}
              </span>
            ))
          : lines.join(" · ")}
      </span>
      {remainder > 0 && (
        /* COUNTED, where this page's two truncation notices are deliberately
         * COUNTLESS forty lines below ("More nets are live than are shown").
         * Not an inconsistency to harmonize in either direction: there the
         * client does not know the server's collection ceiling and must not
         * learn it from copy; here it holds the whole `connections` array and
         * the number is simply true. */
        <a
          data-testid="connection-overflow"
          href={netPermalink(item.linkToken)}
          /* Named per NET. Every capped row would otherwise offer the same
           * "+1 more" to a screen reader, and axe has no rule for links
           * sharing an accessible name — a green scan is not evidence here.
           * The occurrence id carries the uniqueness the title cannot: two
           * nets are free to share a title, and then the names collide again.
           * It is the same id this list already keys these rows by. */
          aria-label={`${overflowLabel(remainder)} into ${item.title} (${item.occurrenceId})`}
          style={overflowLinkStyle}
        >
          {overflowLabel(remainder)}
        </a>
      )}
      {matched !== undefined && (
        <span
          data-testid="matched-connection"
          data-connection-id={matched.id}
          aria-label={`Matches your filter: ${connectionSummary([matched])}`}
          style={matchedConnectionStyle}
        >
          match: {connectionSummary([matched])}
        </span>
      )}
    </span>
  );
}

// One spelling for both the visible text and the accessible name, so the two
// cannot drift into saying different numbers.
function overflowLabel(remainder: number): string {
  return `+${String(remainder)} more ${remainder === 1 ? "way" : "ways"}`;
}

/** The "Live" pill (`--live-*` tokens) with its glowing dot. */
function LiveBadge(): ReactElement {
  return (
    <span style={liveBadgeStyle}>
      <span aria-hidden="true" style={pulseDotStyle} />
      Live
    </span>
  );
}

/**
 * The identity band: what NetRoll is, for a visitor who arrived without an
 * account, plus a status strip counting what is on the air right now. Counts
 * come from the loaded response — while the fetch is in flight the strip says
 * so rather than claiming a zero it cannot know yet.
 */
function HeroBand({
  liveCount,
  liveMore,
  upcomingCount,
  upcomingMore,
}: {
  liveCount: number | null;
  /** Whether the server cut `activeNow`: the count then reads
   * `N+`, because a bare `N` would state a total the response did not. */
  liveMore: boolean;
  upcomingCount: number | null;
  upcomingMore: boolean;
}): ReactElement {
  return (
    <div data-testid="discovery-hero" style={heroStyle}>
      <Logomark size={44} />
      <div>
        <div style={kickerStyle}>Live net logging · in the browser</div>
        <h2 style={pitchStyle}>Run a net, or check in from anywhere.</h2>
        <div data-testid="discovery-status-strip" style={statusStripStyle}>
          {liveCount === null || upcomingCount === null ? (
            <span>Checking what&apos;s on the air…</span>
          ) : (
            <>
              <span aria-hidden="true" style={pulseDotStyle} />
              <span>
                <span data-testid="live-count" style={statusCountStyle}>
                  {liveCount}
                  {liveMore && "+"}
                </span>{" "}
                live now
              </span>
              <span aria-hidden="true">·</span>
              <span>
                <span data-testid="upcoming-count" style={statusCountStyle}>
                  {upcomingCount}
                  {upcomingMore && "+"}
                </span>{" "}
                upcoming
              </span>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

/** The featured "on the air now" entry: the first active net, full treatment.
 * `showFavorite` mirrors the app's favorite convention (PublicNetPage /
 * MyNetsPage) — favoriting requires a signed-in account. */
function FeaturedLiveCard({
  item,
  favorited,
  showFavorite,
  onToggleFavorite,
}: {
  item: DiscoveryNet;
  favorited: boolean;
  showFavorite: boolean;
  onToggleFavorite: (next: boolean) => Promise<void>;
}): ReactElement {
  const facets = [item.netCategory, item.netType]
    .filter((f) => f !== "")
    .join(" · ");
  return (
    <div style={featuredStyle}>
      <div style={featuredHeaderStyle}>
        <div>
          <h3 style={featuredTitleStyle}>
            <a href={netPermalink(item.linkToken)} className={NET_TITLE_LINK_CLASS}>
              {item.title}
            </a>
          </h3>
          <div style={{ marginTop: "var(--space-2)" }}>
            <ConnectionPill item={item} />
          </div>
          {facets !== "" && (
            <div style={{ ...metaStyle, marginTop: "var(--space-2)" }}>
              {facets}
            </div>
          )}
        </div>
        <LiveBadge />
      </div>
      <div style={featuredActionsStyle}>
        <a href={`/live/${item.occurrenceId}`} style={watchLiveLinkStyle}>
          Watch live
        </a>
        <a href={`/live/${item.occurrenceId}`} style={checkInLinkStyle}>
          Check in
        </a>
        {showFavorite && (
          <FavoriteToggle favorited={favorited} onToggle={onToggleFavorite} />
        )}
      </div>
    </div>
  );
}

/** A compact "also live" row for every active net beyond the featured one. */
function CompactLiveRow({ item }: { item: DiscoveryNet }): ReactElement {
  return (
    <div style={compactRowStyle}>
      <div>
        <a
          href={netPermalink(item.linkToken)}
          className={NET_TITLE_LINK_CLASS}
          style={rowTitleStyle}
        >
          {item.title}
        </a>
        <div style={{ marginTop: "var(--space-1)" }}>
          <ConnectionPill item={item} />
        </div>
      </div>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: "var(--space-3)",
        }}
      >
        <LiveBadge />
        <a href={`/live/${item.occurrenceId}`}>Watch ›</a>
      </div>
    </div>
  );
}

/** A single upcoming-net row. In row mode it lays out on the panel's shared
 * column template; on phone widths it stacks. The net cell carries
 * the title, the facet/geography meta line, and — when the net has one — its
 * description as a two-line clamped preview, so two similarly-titled nets are
 * tellable apart without opening both. Time is humanized relative to `now`
  */
function UpcomingRow({
  item,
  position,
  now,
  stacked,
  favorited,
  showFavorite,
  onToggleFavorite,
}: {
  item: DiscoveryNet;
  position: number;
  now: Date;
  stacked: boolean;
  favorited: boolean;
  showFavorite: boolean;
  onToggleFavorite: (next: boolean) => Promise<void>;
}): ReactElement {
  const geography = [item.grid, item.state, item.country]
    .filter((part): part is string => part !== null && part !== "")
    .join(" · ");
  const facets = [item.netCategory, item.netType]
    .filter((f) => f !== "")
    .join(" · ");
  const detail = [facets, geography]
    .filter((part) => part !== "")
    .concat(
      item.expectedDurationMinutes === null
        ? []
        : [`runs ~${item.expectedDurationMinutes} min`],
    )
    .join(" · ");

  return (
    <li style={stacked ? rowStackedStyle : rowGridStyle}>
      {!stacked && (
        <span aria-hidden="true" style={rowPosStyle}>
          {position}
        </span>
      )}
      <div style={{ minWidth: 0 }}>
        <a
          href={netPermalink(item.linkToken)}
          className={NET_TITLE_LINK_CLASS}
          style={rowTitleStyle}
        >
          {item.title}
        </a>
        {detail !== "" && <div style={metaStyle}>{detail}</div>}
        <DescriptionPreview description={item.description} />
      </div>
      {/* The ONLY bounded site: the one pill that is itself a grid item in a
          track that cannot grow. The featured card, the compact live row and
          the next-up strip have room, and capping them would hide facts to
          solve a problem they do not have. */}
      <ConnectionPill item={item} bounded />
      {/* The relative phrase alone ("in 4d") cannot be checked against
       * anything, which is how a recurrence rule stored on the wrong weekday
       * went unnoticed on the live instance. The absolute
       * local time — weekday included — travels with it. The raw UTC instant
       * stays in `dateTime`, never as visible text. */}
      <time dateTime={item.scheduledStartAt} style={rowTimeStyle}>
        {humanizeTime(item.scheduledStartAt, now)}
        <span style={rowTimeAbsoluteStyle}>
          {absoluteLocalTimeWithWeekday(item.scheduledStartAt)}
        </span>
      </time>
      {showFavorite && (
        <FavoriteToggle favorited={favorited} onToggle={onToggleFavorite} />
      )}
    </li>
  );
}

/**
 * The nets whose star is on screen for `data`: the featured live net and every
 * upcoming row — the compact live rows carry no star. Deduplicated, because one
 * definition can be live now and scheduled again later.
 */
function starredNetIds(data: DiscoveryResponse): string[] {
  const featured = data.activeNow.length === 0 ? [] : [data.activeNow[0].id];
  return [...new Set([...featured, ...data.upcoming.map((item) => item.id)])];
}

/**
 * The public discovery landing (app root): an identity band
 * for account-less first visitors, the on-air panel (with a next-up pointer
 * when nothing is live), and the upcoming panel behind a filter toolbar. A
 * public, account-less read: no auth, plain `fetch` via `getDiscovery`.
 * Fetch lifecycle is loading → success/empty → error+retry with NO auto-refresh
 */
export function DiscoveryPage(): ReactElement {
  // The filter set is shareable, so it lives in the URL. Seeded from
  // it with a LAZY initialiser rather than an effect — an effect would fire a
  // second request, and the very first `load(filters)` below has to already
  // carry what the link asked for.
  const [searchParams, setSearchParams] = useSearchParams();
  const [filters, setFilters] = useState<DiscoveryFilters>(() =>
    filtersFromSearchParams(searchParams),
  );
  const [state, setState] = useState<LoadState>({ status: "loading" });
  const [filtersExpanded, setFiltersExpanded] = useState(false);
  const now = new Date();
  const { mode } = useLayout();
  const stacked = mode === "stacked-card";

  // Favoriting is an authenticated action everywhere else in the app
  // (PublicNetPage, MyNetsPage) — the discovery landing follows the same
  // convention: the star only appears once signed in, seeded from the
  // account's real favorites rather than assumed unfavorited.
  const { account } = useCurrentAccount();
  const [favoritedIds, setFavoritedIds] = useState<Set<string>>(new Set());
  // The nets whose star the viewer toggled while a membership read was in
  // flight. That read describes the server as it stood when it BEGAN, so for
  // these ids the confirmed toggle is the newer fact and the answer must not
  // put the star back; every other id on the page still takes the answer.
  // `cancelled` cannot do this — it covers unmount and a changed dependency,
  // and a toggle is neither.
  const toggledDuringReconcile = useRef<Set<string>>(new Set());

  useEffect(() => {
    if (account === null) {
      setFavoritedIds(new Set());
      return;
    }
    if (state.status !== "success") {
      return;
    }
    // Asked about the nets on THIS page, not the account's whole collection:
    // one request whatever the favorite count, on every load of the list.
    const asked = starredNetIds(state.data);
    let cancelled = false;
    toggledDuringReconcile.current = new Set();
    fetchFavoriteMembership(asked)
      .then((favorited) => {
        if (cancelled) {
          return;
        }
        const toggled = toggledDuringReconcile.current;
        setFavoritedIds((prev) => {
          const updated = new Set(prev);
          for (const id of asked) {
            if (toggled.has(id)) {
              continue;
            }
            if (favorited.has(id)) {
              updated.add(id);
            } else {
              updated.delete(id);
            }
          }
          return updated;
        });
      })
      .catch(() => {
        // Leave the stars as they are; nothing to reconcile with.
      });
    return () => {
      cancelled = true;
    };
  }, [account, state]);

  const toggleFavorite =
    (netId: string) =>
    async (next: boolean): Promise<void> => {
      if (next) {
        await favoriteNet(netId);
      } else {
        await unfavoriteNet(netId);
      }
      // Reflect only after the server confirms the write — and outrank any
      // membership answer still in flight, which predates it.
      toggledDuringReconcile.current.add(netId);
      setFavoritedIds((prev) => {
        const updated = new Set(prev);
        if (next) {
          updated.add(netId);
        } else {
          updated.delete(netId);
        }
        return updated;
      });
    };

  // Both URL effects below read the current render's values through this ref
  // instead of naming them as dependencies. That is the only reason the two can
  // coexist: the debounce must not re-arm when the URL changes (it would fire a
  // second request for a state already loaded), and the resync must not re-run
  // when `filters` changes (it would overwrite keystrokes with the pre-debounce
  // URL — the search box appearing to clear itself while you type).
  const latest = useRef({ filters, searchParams });
  latest.current = { filters, searchParams };

  // The canonical query string the page believes the address bar already shows.
  // React Router commits a navigation inside a transition, so the page's OWN
  // push reaches `searchParams` one or more renders late — by which time the
  // viewer may already have changed another filter. Without this, that late
  // arrival looks exactly like a back-navigation to the resync below and
  // overwrites the newer edit. Updated on both sides of the sync, so a genuine
  // forward-navigation back to a previously-pushed state is still honoured.
  const syncedQuery = useRef(
    discoverySearchParams(filtersFromSearchParams(searchParams)).toString(),
  );

  const load = useCallback((f: DiscoveryFilters) => {
    setState({ status: "loading" });
    getDiscovery(f)
      .then((data) => setState({ status: "success", data }))
      .catch((error: unknown) =>
        setState({
          status: "error",
          problem: error instanceof ProblemError ? error.problem : undefined,
        }),
      );
  }, []);

  // Load immediately on mount; debounce every subsequent filter-driven
  // re-fetch (never a polling timer — still user-driven, just coalesced) so
  // a burst of filter/text-input changes settles into ONE request instead of
  // one per keystroke (see `FILTER_DEBOUNCE_MS`).
  const isFirstRender = useRef(true);
  useEffect(() => {
    if (isFirstRender.current) {
      isFirstRender.current = false;
      load(filters);
      return;
    }
    const timer = setTimeout(() => {
      // The URL changes at the same moment the request does: one settled
      // change, one history entry, one fetch. Both sides of the comparison are
      // CANONICAL — comparing against `searchParams.toString()` raw would read
      // a differently-ordered arrival URL, or one carrying an `fbclid`, as a
      // change and push an entry that silently strips the viewer's link.
      const next = discoverySearchParams(filters).toString();
      const current = discoverySearchParams(
        filtersFromSearchParams(latest.current.searchParams),
      ).toString();
      if (next !== current) {
        syncedQuery.current = next;
        // `setSearchParams` is captured from the render that armed this timer,
        // and is deliberately NOT a dependency: it is `useCallback`'d on
        // `searchParams`, so naming it would re-arm the timer on the page's own
        // push and fire a second request. Safe to capture stale because a string
        // argument never reads the previous params.
        setSearchParams(next);
      }
      load(filters);
    }, FILTER_DEBOUNCE_MS);
    return () => clearTimeout(timer);
    // `setSearchParams` is omitted deliberately and must stay omitted: it is
    // `useCallback`'d on `searchParams`, so listing it re-arms this timer on the
    // page's own push and fires a second request for a state already loaded.
    // Pinned by the call-count assertion in the history test.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filters, load]);

  // Back/forward: the URL moved without the controls moving. Re-seed `filters`
  // from it, which re-arms the debounce above and re-REQUESTS the popped state
  // rather than only re-painting it. `searchParams` is memoised on
  // `location.search`, so this runs on URL changes only, not on every render.
  useEffect(() => {
    const fromUrl = filtersFromSearchParams(searchParams);
    const canonical = discoverySearchParams(fromUrl).toString();
    if (canonical === syncedQuery.current) {
      return;
    }
    syncedQuery.current = canonical;
    if (canonical !== discoverySearchParams(latest.current.filters).toString()) {
      setFilters(fromUrl);
    }
  }, [searchParams]);

  const set = (key: keyof DiscoveryFilters) => (value: string) =>
    setFilters((prev) => ({ ...prev, [key]: value }));

  // Each filter is a single chip-shaped control: the currently selected option
  // or typed text is the chip's own label, so the standalone caption span goes
  // away — `aria-label` alone carries the accessible name.
  //
  // `labels`, when given, is what each option is CALLED; the option's value
  // stays the wire token either way. The band/mode/category/type vocabularies
  // read as themselves, so they pass nothing; the connection kinds do not (no
  // visitor knows what `urf` is), so they pass `KIND_LABELS`.
  const selectField = (
    label: string,
    key: keyof DiscoveryFilters,
    options: readonly string[],
    labels?: Readonly<Record<string, string>>,
  ): ReactElement => {
    const value = filters[key] ?? "";
    return (
      <select
        key={key}
        aria-label={label}
        value={value}
        onChange={(event) => set(key)(event.target.value)}
        style={value === "" ? chipStyle : chipActiveStyle}
      >
        <option value="">{label}</option>
        {options.map((option) => (
          <option key={option} value={option}>
            {labels?.[option] ?? option}
          </option>
        ))}
      </select>
    );
  };

  const textField = (
    label: string,
    key: keyof DiscoveryFilters,
  ): ReactElement => {
    const value = filters[key] ?? "";
    return (
      <input
        key={key}
        type="text"
        aria-label={label}
        placeholder={label}
        value={value}
        onChange={(event) => set(key)(event.target.value)}
        style={value === "" ? chipStyle : chipActiveStyle}
      />
    );
  };

  const secondaryFields: readonly ReactElement[] = [
    selectField("Category", "category", CATEGORIES),
    selectField("Net type", "type", NET_TYPES),
    textField("Country", "country"),
    textField("State", "state"),
    textField("Grid", "grid"),
    selectField("Sort", "sort", SORTS),
  ];

  // A filter that is set but scrolled behind the disclosure is still
  // narrowing the list; the count is what keeps that from reading as a bug.
  //
  // This stays CLIENT-derived. It is a
  // badge on a control, answering "a hidden control still holds a value", not
  // "the server narrowed by this much" — and driving it from the response would
  // make it lag every change by the 300 ms debounce and flicker on each load,
  // which is the same trap that keeps the `<select>`/`<input>` values on client
  // state. The server's side of that question is stated separately, and
  // honestly, by the applied-query echo below.
  const collapsedActiveCount = Object.entries(filters).filter(
    ([key, value]) =>
      value !== undefined &&
      value !== "" &&
      !(PRIMARY_FILTER_KEYS as readonly string[]).includes(key),
  ).length;

  // What the SERVER applied, taken from the response and never from
  // `filters`. These are two different facts — `filters` is what this page hopes
  // it sent, and on a lenient endpoint a misspelled or unhonoured key never
  // reaches this list. Deliberately NOT wired to the controls' `value` props:
  // those stay on client state, because sourcing them from the last response
  // would lag every keystroke by the 300 ms debounce.
  //
  // Read defensively even though the interface makes `applied` required: this
  // value is unvalidated network JSON, and a `TypeError` here is thrown during
  // render, which takes the whole route down through the error boundary. An
  // absent echo degrades to "no statement", never to a blank page.
  const applied = state.status === "success" ? state.data.applied : undefined;
  // Two guards, and NEITHER is defensive tidiness — each answers a value that
  // reaches React as a child and throws during render, taking the whole route
  // down through the error boundary.
  //
  // 1. The container must be a plain object before it is enumerated. A response
  // whose `applied` is a string or an array is nonsense, and enumerating it
  // would FABRICATE a statement out of its indices (`applied: "time"` reads
  // as `0: t | 1: i | …`) under an accessible name promising the server's
  // answer. Nonsense degrades to no statement.
  // 2. `typeof value === "string"` per entry. Narrowing the value is necessary
  // and not sufficient: `appliedLabel` above must resolve the LABEL by own
  // property for the same reason, or a `__proto__`/`toString` key walks the
  // prototype chain and puts a non-string back in the tree past this guard.
  //
  // Known keys first, in list order; then any further dimension the response
  // states, so the client's literal being short never silently drops what the
  // server says it did.
  const appliedEntries: readonly (readonly [string, string])[] =
    applied === null || typeof applied !== "object" || Array.isArray(applied)
      ? []
      : [
          ...FILTER_KEYS.flatMap((key) => {
            const value = applied[key];
            return typeof value === "string" && value !== ""
              ? [[key, value] as const]
              : [];
          }),
          ...Object.entries(applied).flatMap(([key, value]) =>
            !(FILTER_KEYS as readonly string[]).includes(key) &&
            key !== SORT_UNAVAILABLE_KEY &&
            key !== TRUNCATED_KEY &&
            typeof value === "string" &&
            value !== ""
              ? [[key, value] as const]
              : [],
          ),
        ];

  // The first key excluded from the extra-dimension loop above,
  // by name (`TRUNCATED_KEY` is the second). It is not a dimension
  // the server APPLIED — it is the one it could not — and this list's
  // accessible name promises the former. Excluding by name rather than by
  // shape leaves the extra-key path open for every future dimension, which is
  // the whole point of that loop.
  const sortUnavailable =
    applied === null ||
    typeof applied !== "object" ||
    Array.isArray(applied) ||
    typeof applied[SORT_UNAVAILABLE_KEY] !== "string" ||
    applied[SORT_UNAVAILABLE_KEY] === ""
      ? undefined
      : applied[SORT_UNAVAILABLE_KEY];

  // The collections the server CUT, the other key excluded from
  // the loop above by name. Derived with the same posture as `sortUnavailable`
  // — the value is unvalidated network JSON, and anything but an array of
  // strings is NO statement rather than a render throw. An absent key is the
  // server's positive statement that nothing was cut; nothing here defaults
  // it, because there is nothing to default.
  const truncatedValue =
    applied === null || typeof applied !== "object" || Array.isArray(applied)
      ? undefined
      : applied[TRUNCATED_KEY];
  const truncatedCollections: readonly string[] =
    Array.isArray(truncatedValue) &&
    truncatedValue.every((c): c is string => typeof c === "string")
      ? truncatedValue
      : [];
  // A name this client does not recognise is deliberately silent, and the
  // reason is structural rather than an exception to the unknown-key rule
  // stated for `appliedEntries` above. That rule
  // works because the applied list is FLAT — an unknown key is appended to it
  // and labelled by itself. A truncation statement is POSITIONAL: it belongs
  // beside the panel whose collection it describes, and an unrecognised
  // collection has no panel to sit beside. Matching is case-sensitive against
  // the wire literals; the client does not normalise a vocabulary it does not
  // own.
  const activeNowTruncated = truncatedCollections.includes(ACTIVE_NOW_COLLECTION);
  const upcomingTruncated = truncatedCollections.includes(UPCOMING_COLLECTION);

  // The echo is a POSITIVE statement and structurally cannot name a key it
  // never saw — `?bnad=40m` and no `bnad` produce identical echoes. The page can
  // answer that, because it has a closed vocabulary the server does not: the
  // difference is taken against `FILTER_KEYS` and NEVER against `applied`.
  // Diffing against the echo would report `?band=` — a known key, blank,
  // correctly absent — as unhonoured when the page asked for nothing. Read from
  // the live `searchParams`, so it disappears by construction the moment the
  // viewer edits a filter and the page rewrites the URL.
  const ignoredParamKeys = [
    ...new Set(
      [...searchParams.keys()].filter(
        (key) => !(FILTER_KEYS as readonly string[]).includes(key),
      ),
    ),
  ];

  const activeNow = state.status === "success" ? state.data.activeNow : [];
  const upcoming = state.status === "success" ? state.data.upcoming : [];
  const loaded = state.status === "success";
  const nextUp = activeNow.length === 0 ? upcoming[0] : undefined;

  return (
    <div style={pageStyle}>
      <Breadcrumb items={[{ label: "Nets" }]} />

      <HeroBand
        liveCount={loaded ? activeNow.length : null}
        liveMore={activeNowTruncated}
        upcomingCount={loaded ? upcoming.length : null}
        upcomingMore={upcomingTruncated}
      />

      <Panel
        title="On the air now"
        headerAside={
          activeNow.length > 0 ? (
            <span data-testid="live-panel-count" style={metaStyle}>
              {activeNow.length}
              {activeNowTruncated && "+"} live
            </span>
          ) : undefined
        }
      >
        {state.status === "loading" ? (
          <div style={bodyPadStyle}>
            {/* A distinct loading placeholder — NOT the confirmed-empty
             * "No nets are live right now.", which asserts a fact the fetch
             * hasn't established yet. */}
            <p style={{ ...metaStyle, margin: 0 }}>Checking for live nets…</p>
          </div>
        ) : activeNow.length === 0 ? (
          /* A CUT collection that came back
           * empty suppresses this whole branch. "No nets are live right now."
           * is a totality claim, and the sibling statement below says more are
           * live than are shown — leaving both up states both halves of a
           * contradiction. `nextUp` goes with it rather than separately,
           * because its entire premise is that `activeNow.length === 0` means
           * nothing is on the air, which is exactly what is false here. The
           * truncation statement then stands alone in the panel: no new copy,
           * and the same placement rule that applies to the with-cards case. */
          activeNowTruncated ? null : (
            <div style={bodyPadStyle}>
              <p data-testid="active-now-empty" style={{ margin: 0 }}>
                No nets are live right now.
              </p>
              {nextUp !== undefined && (
                <div data-testid="next-up" style={nextUpStyle}>
                  <span>Next up</span>
                  <a
                    href={netPermalink(nextUp.linkToken)}
                    className={NET_TITLE_LINK_CLASS}
                    style={rowTitleStyle}
                  >
                    {nextUp.title}
                  </a>
                  <ConnectionPill item={nextUp} />
                  <time dateTime={nextUp.scheduledStartAt} style={rowTimeStyle}>
                    {humanizeTime(nextUp.scheduledStartAt, now)}
                    <span style={rowTimeAbsoluteStyle}>
                      {absoluteLocalTimeWithWeekday(nextUp.scheduledStartAt)}
                    </span>
                  </time>
                </div>
              )}
            </div>
          )
        ) : (
          <>
            <FeaturedLiveCard
              item={activeNow[0]}
              favorited={favoritedIds.has(activeNow[0].id)}
              showFavorite={account !== null}
              onToggleFavorite={toggleFavorite(activeNow[0].id)}
            />
            {activeNow.slice(1).map((item) => (
              <CompactLiveRow key={item.occurrenceId} item={item} />
            ))}
          </>
        )}
        {/* The server cut this collection. Rendered ALONGSIDE the
         * cards, never instead of them — the cards shown are real, the
         * statement only says they are not all of them — and independent of
         * how many were served, because a cut collection may legitimately
         * come back short, even empty, when a card inside the window could not
         * be read. The NEUTRAL form is deliberate: nothing
         * narrows `activeNow`, so it must not say "narrow", and it does not go
         * on to tell the visitor the rest is unreachable — that limitation is
         * paid for by the server's ceiling, not carried by the reader. */}
        {activeNowTruncated && (
          <div style={bodyPadStyle}>
            <p
              role="status"
              data-testid="active-now-truncated"
              style={{ ...metaStyle, margin: 0 }}
            >
              More nets are live than are shown.
            </p>
          </div>
        )}
      </Panel>

      <Panel title="Upcoming">
        <div style={toolbarStyle}>
          {textField("Search", "q")}
          {selectField("Band", "band", BANDS)}
          {selectField("Mode", "mode", MODES)}
          {/* The label is PINNED: this file's tests address the band and mode
           * chips by /band/i and /mode/i, which throw on a second match, so any
           * label containing either word breaks a double-digit number of them
           * at once. "Connection kind" collides with none. */}
          {selectField("Connection kind", "kind", CONNECTION_KINDS, KIND_LABELS)}
          <button
            type="button"
            aria-expanded={filtersExpanded}
            onClick={() => setFiltersExpanded((prev) => !prev)}
            style={disclosureStyle}
          >
            More filters
            {collapsedActiveCount > 0 && (
              <span style={activeCountStyle}>{collapsedActiveCount}</span>
            )}
          </button>
        </div>

        {/* Kept mounted-but-hidden: collapsing must not silently discard the
         * filter values still shaping the list (hence the count above). */}
        <div
          hidden={!filtersExpanded}
          style={filtersExpanded ? secondaryFiltersStyle : undefined}
        >
          {secondaryFields}
        </div>

        {appliedEntries.length > 0 && (
          <div style={appliedBarStyle}>
            <span style={metaStyle}>Showing</span>
            <ul
              aria-label="Filters and sort the server applied"
              data-testid="applied-summary"
              style={appliedListStyle}
            >
              {appliedEntries.map(([key, value]) => (
                <li
                  key={key}
                  data-testid={`applied-${key}`}
                  data-applied-key={key}
                  style={appliedItemStyle}
                >
                  {appliedLabel(key)}: {value}
                </li>
              ))}
            </ul>
          </div>
        )}

        {/* A THIRD bar rather than an entry in "Showing" above: that list's
         * accessible name promises the filters and sort the server APPLIED, and
         * this is the one it could not. No `aria-label` on the statement itself
         * — one would REPLACE the sentence for a screen reader and drop the very
         * token the statement exists to name. */}
        {sortUnavailable !== undefined && (
          <div style={appliedBarStyle}>
            <span style={metaStyle}>Sort</span>
            <span
              data-testid="sort-unavailable"
              data-sort-unavailable={sortUnavailable}
              style={appliedItemStyle}
            >
              “{sortUnavailable}” is no longer offered — a net can be on several.
              Showing soonest first.
            </span>
          </div>
        )}

        {ignoredParamKeys.length > 0 && (
          <div style={appliedBarStyle}>
            <span style={metaStyle}>Not used by this page</span>
            <ul
              aria-label="Parameters in this link that this page does not use"
              data-testid="ignored-params"
              style={appliedListStyle}
            >
              {ignoredParamKeys.map((key) => (
                <li
                  key={key}
                  data-testid={`ignored-${key}`}
                  data-ignored-key={key}
                  style={appliedItemStyle}
                >
                  {key}
                </li>
              ))}
            </ul>
          </div>
        )}

        {loaded && upcoming.length > 0 && !stacked && (
          <PanelColumnHeads
            template={UPCOMING_TEMPLATE}
            labels={[
              "#",
              "Net",
              "Frequency",
              "Starts",
              ...(account === null ? [] : [""]),
            ]}
          />
        )}

        {state.status === "loading" && (
          <div style={bodyPadStyle}>
            <p role="status" style={{ ...metaStyle, margin: 0 }}>
              Loading nets…
            </p>
          </div>
        )}
        {state.status === "error" && (
          <div style={bodyPadStyle}>
            <p role="alert" style={{ ...errorStyle, margin: 0 }}>
              {messageForProblem(state.problem)}
            </p>
            <button
              type="button"
              onClick={() => load(filters)}
              style={secondaryButtonStyle}
            >
              Try again
            </button>
          </div>
        )}
        {loaded &&
          (upcoming.length === 0 ? (
            /* Suppressed when `upcoming` was cut,
             * for the reason above — "No upcoming nets." is the strongest
             * totality claim on this page and the statement below contradicts
             * it. */
            upcomingTruncated ? null : (
              <div style={bodyPadStyle}>
                <p data-testid="upcoming-empty" style={{ margin: 0 }}>
                  No upcoming nets.
                </p>
              </div>
            )
          ) : (
            <ResponsiveList>
              <ul style={{ listStyle: "none", padding: 0, margin: 0 }}>
                {upcoming.map((item, index) => (
                  <UpcomingRow
                    key={item.occurrenceId}
                    item={item}
                    position={index + 1}
                    now={now}
                    stacked={stacked}
                    favorited={favoritedIds.has(item.id)}
                    showFavorite={account !== null}
                    onToggleFavorite={toggleFavorite(item.id)}
                  />
                ))}
              </ul>
            </ResponsiveList>
          ))}
        {/* The ACTIONABLE form: the filters above genuinely reach
         * the rest, so this one may say so where `activeNow`'s may not. "More
         * … than are shown", never "hit the cap": exactly the cap is complete,
         * and the client does not know the bound and must not learn it from
         * copy. */}
        {upcomingTruncated && (
          <div style={bodyPadStyle}>
            <p
              role="status"
              data-testid="upcoming-truncated"
              style={{ ...metaStyle, margin: 0 }}
            >
              More upcoming nets match than are shown. Narrow the filters to
              reach the rest.
            </p>
          </div>
        )}
      </Panel>
    </div>
  );
}
