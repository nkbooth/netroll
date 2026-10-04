// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent, ReactElement } from "react";
import { useNavigate } from "react-router";

import { messageForProblem } from "../../errors/problemMessages";
import { Breadcrumb } from "../../ui/components/Breadcrumb";
import { DescriptionPreview } from "../../ui/components/DescriptionPreview";
import { Panel } from "../../ui/components/Panel";
import { FavoriteToggle } from "../../ui/components/FavoriteToggle";
import { tokens } from "../../ui/tokens/tokens";
import { humanizeTime } from "../../ui/util/humanizeTime";
import type { Account, Problem } from "../auth/authApi";
import { ProblemError } from "../auth/authApi";
import { fetchCurrentAccount } from "../auth/authApi";
import { startSession } from "../session/sessionApi";
import { getMyNets, unfavoriteNet } from "./favoritesApi";
import type { FavoriteNet } from "./favoritesApi";
import { connectionSummary } from "./connectionPresentation";
import { netPermalink } from "./netPermalink";
import { getOwnedNets } from "./netsApi";
import type { OwnedNet } from "./netsApi";

/**
 * One tab's accumulated list. `rows` grows by appending each
 * fetched page; `nextCursor` is `null` once the walk is done — the
 * `ProfilePage` check-in-history shape.
 */
type ListState<T> =
  | { status: "loading" }
  | { status: "success"; rows: T[]; nextCursor: string | null }
  | { status: "error"; problem?: Problem };

/** One page of a keyset-paginated list read, as both tab reads serve it. */
interface ListPage<T> {
  items: T[];
  nextCursor: string | null;
}

/**
 * Whether a loaded list is genuinely empty: no rows AND nothing left to load.
 * "No rows" alone is not that. A page can legitimately come back empty with a
 * cursor — the server splits the page before it skips damaged rows, so the
 * cursor always advances — and unfavoriting every loaded row leaves the same
 * shape. Both still have a rest of the list to reach, so both keep the
 * "Load more" footer rather than showing "you have none".
 */
function isExhaustedEmpty(state: { rows: unknown[]; nextCursor: string | null }): boolean {
  return state.rows.length === 0 && state.nextCursor === null;
}

type TabKey = "owned" | "favorites";

const pageStyle: CSSProperties = {
  // The content measure the shell chrome shares (DESIGN.md § Layout).
  maxWidth: "1200px",
  margin: "0 auto",
  padding: "var(--space-6) var(--space-page-x)",
  fontSize: tokens.typography.body.fontSize,
  lineHeight: tokens.typography.body.lineHeight,
};

const panelStyle: CSSProperties = {
  marginTop: "var(--space-4)",
};

const headerRowStyle: CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  alignItems: "center",
  gap: "var(--space-4)",
  flexWrap: "wrap",
  padding: "var(--space-5) var(--space-row-x)",
  background: "var(--head-grad)",
  borderBottom: "1px solid var(--border)",
};

const headingStyle: CSSProperties = {
  fontSize: tokens.typography.sessionTitle.fontSize,
  fontWeight: tokens.typography.sessionTitle.fontWeight,
  letterSpacing: tokens.typography.sessionTitle.letterSpacing,
  margin: 0,
};

const newNetLinkStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
  background: "var(--accent-deep)",
  color: "var(--on-accent)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-3)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 800,
  textDecoration: "none",
};

const tabListStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-5)",
  padding: "var(--space-3) var(--space-row-x) 0",
  background: "var(--surface-2)",
  borderBottom: "1px solid var(--border)",
};

function tabButtonStyle(active: boolean): CSSProperties {
  return {
    background: "transparent",
    border: "none",
    borderBottom: active ? "2px solid var(--accent)" : "2px solid transparent",
    color: active ? "var(--text)" : "var(--text-muted)",
    font: "inherit",
    fontSize: tokens.typography.meta.fontSize,
    fontWeight: 700,
    padding: "0 0 var(--space-3)",
    cursor: "pointer",
  };
}

const tabCountStyle: CSSProperties = {
  color: "var(--text-muted)",
  fontWeight: 600,
  marginLeft: "var(--space-1)",
};

/** The rows inside a tab panel run flush to the panel frame and carry their
 * own padding, so the panel itself adds none. */
const tabPanelStyle: CSSProperties = {};

/** Padding for a tab panel's non-row content (loading/error/empty states). */
const tabMessageStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-row-x)",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
};

const metaStyle: CSSProperties = {
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
};

const monoFreqStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  color: "var(--freq-text)",
};

const monoTimeStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  color: "var(--text)",
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
  marginTop: "var(--space-3)",
};

const secondaryButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-3)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
  marginTop: "var(--space-2)",
};

const listStyle: CSSProperties = { listStyle: "none", padding: 0, margin: 0 };

const ownedListStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
};

/** An owned-net row. Flush inside the panel and divided by a hairline — the
 * 4px accent left-bar still carries "live" (DESIGN.md state language), which is
 * the one thing the removed per-row card was doing that mattered. */
function ownedRowStyle(live: boolean): CSSProperties {
  return {
    padding: "var(--space-row-y) var(--space-row-x)",
    borderBottom: "1px solid var(--border)",
    borderLeft: live
      ? "var(--space-status-bar) solid var(--accent)"
      : undefined,
  };
}

const ownedCardHeadStyle: CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  alignItems: "flex-start",
  gap: "var(--space-3)",
  flexWrap: "wrap",
};

const ownedTitleRowStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  flexWrap: "wrap",
};

const ownedTitleStyle: CSSProperties = {
  fontSize: "16px",
  fontWeight: 700,
};

const liveBadgeStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
  borderRadius: "var(--rounded-full)",
  padding: "3px 10px 3px 8px",
  fontSize: "11px",
  fontWeight: 800,
  border: "1px solid var(--live-border)",
  background: "var(--live-fill)",
  color: "var(--live-text)",
};

const liveDotStyle: CSSProperties = {
  width: "7px",
  height: "7px",
  borderRadius: "var(--rounded-full)",
  background: "var(--live-dot)",
  display: "inline-block",
};

const ownedActionsRowStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  flexWrap: "wrap",
};

const primaryButtonStyle: CSSProperties = {
  background: "var(--accent-deep)",
  border: "none",
  color: "var(--on-accent)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-3)",
  fontSize: "12px",
  fontWeight: 800,
  font: "inherit",
  cursor: "pointer",
  textDecoration: "none",
  display: "inline-flex",
  alignItems: "center",
};

const outlineButtonStyle: CSSProperties = {
  background: "transparent",
  border: "1px solid var(--accent-deep)",
  color: "var(--accent-ink)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-3)",
  fontSize: "12px",
  fontWeight: 800,
  font: "inherit",
  cursor: "pointer",
};

const neutralButtonStyle: CSSProperties = {
  background: "transparent",
  border: "1px solid var(--border)",
  color: "var(--text)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-3)",
  fontSize: "12px",
  fontWeight: 700,
  font: "inherit",
  cursor: "pointer",
  textDecoration: "none",
  display: "inline-flex",
  alignItems: "center",
};

const iconButtonStyle: CSSProperties = {
  background: "transparent",
  border: "1px solid var(--border)",
  color: "var(--text-muted)",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2)",
  cursor: "pointer",
  display: "inline-flex",
  alignItems: "center",
};

const favoriteRowStyle: CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  alignItems: "center",
  gap: "var(--space-3)",
  flexWrap: "wrap",
  padding: "var(--space-row-y) var(--space-row-x)",
  borderBottom: "1px solid var(--border)",
};

const titleLinkStyle: CSSProperties = {
  fontWeight: 700,
  fontSize: tokens.typography.body.fontSize,
  color: "var(--text)",
};

const archivedBadgeStyle: CSSProperties = {
  ...labelStyle,
  color: "var(--warn)",
};

const emptyFavoritesWrapperStyle: CSSProperties = {
  marginTop: "var(--space-4)",
  border: "1px dashed var(--border)",
  borderRadius: "var(--rounded-xl)",
  padding: "var(--space-6) var(--space-5)",
  textAlign: "center",
};

const emptyIconWrapperStyle: CSSProperties = {
  width: "44px",
  height: "44px",
  borderRadius: "var(--rounded-lg)",
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
};

const emptyTitleStyle: CSSProperties = {
  fontSize: "15px",
  fontWeight: 700,
  marginTop: "var(--space-4)",
};

const emptySubStyle: CSSProperties = {
  ...metaStyle,
  margin: "var(--space-2) 0 var(--space-4)",
};

const emptyButtonStyle: CSSProperties = {
  ...primaryButtonStyle,
  padding: "var(--space-2) var(--space-4)",
  fontSize: "13px",
};

/** A filled star (favorited) — decorative only; the accessible label lives on
 * the empty-state text, not this icon. */
function EmptyFavoritesIcon(): ReactElement {
  return (
    <svg
      aria-hidden="true"
      width="22"
      height="22"
      viewBox="0 0 24 24"
      fill="none"
      stroke="var(--text-muted)"
      strokeWidth="1.8"
    >
      <path d="m12 17.3-6.2 3.7 1.6-7L2 9.2l7.1-.6L12 2l2.9 6.6 7.1.6-5.4 4.8 1.6 7z" />
    </svg>
  );
}

/** Up-arrow-into-a-box "share" glyph — decorative; the button carries its own
 * `aria-label`. */
function ShareIcon(): ReactElement {
  return (
    <svg
      aria-hidden="true"
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
    >
      <path d="M4 12v7a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-7M16 6l-4-4-4 4M12 2v13" />
    </svg>
  );
}

function PlusIcon(): ReactElement {
  return (
    <svg
      aria-hidden="true"
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.4"
    >
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

/** One favorited net, restyled as a bordered surface-2 row (restyle):
 * a link back to its permalink view, a facet line whose band segment carries
 * the mono freq-tinted treatment, the net's description as a two-line clamped
 * preview when it has one, an archived indicator when set, and a toggle to
 * unfavorite (which removes the row on success). */
function MyNetRow({
  favorite,
  now,
  onUnfavorite,
}: {
  favorite: FavoriteNet;
  now: Date;
  onUnfavorite: (id: string) => Promise<void>;
}): ReactElement {
  // The band segment used to read the definition's flat `band`/`mode` mirror,
  // which went stale for an internet-only net; the summary reads the connection
  // set now, exactly as the Owned card and the discovery card do.
  const rest = [favorite.netCategory, favorite.netType]
    .filter((f) => f !== "")
    .join(" · ");

  return (
    <li style={favoriteRowStyle}>
      {/* `flex: 1, minWidth: 0` is what keeps the unfavorite toggle on the
          title line. Flex line-breaking measures each item at its max-content
          width, and a description's max-content is the whole thing on one line,
          so without a basis the wrapping row pushes the toggle onto a line of
          its own. `overflowWrap: "anywhere"` does not help — it lowers
          min-content, not max-content. */}
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)", flexWrap: "wrap" }}>
          <a href={netPermalink(favorite.linkToken)} style={titleLinkStyle}>
            {favorite.title}
          </a>
          {favorite.archivedAt !== null && (
            <span data-testid="archived-badge" role="status" style={archivedBadgeStyle}>
              Archived
            </span>
          )}
        </div>
        <div style={{ ...metaStyle, marginTop: "var(--space-1)" }}>
          <span style={monoFreqStyle}>{connectionSummary(favorite.connections)}</span>
          {rest !== "" && <> · {rest}</>}
          {" · favorited "}
          <time dateTime={favorite.favoritedAt}>{humanizeTime(favorite.favoritedAt, now)}</time>
        </div>
        <DescriptionPreview description={favorite.description} />
      </div>
      <FavoriteToggle favorited={true} onToggle={() => onUnfavorite(favorite.id)} />
    </li>
  );
}

/** The Favorites tab's empty state (restyle): icon + dashed card +
 * filled accent CTA to Discovery, rather than a single plain paragraph. */
function EmptyFavorites(): ReactElement {
  return (
    <div data-testid="favorites-empty" style={emptyFavoritesWrapperStyle}>
      <span style={emptyIconWrapperStyle}>
        <EmptyFavoritesIcon />
      </span>
      <div style={emptyTitleStyle}>You haven&apos;t favorited any nets yet</div>
      <p style={emptySubStyle}>
        Tap the star on any net to keep it here for quick access.
      </p>
      <a href="/" style={emptyButtonStyle}>
        Browse Discovery
      </a>
    </div>
  );
}

/**
 * One owned net card on the Owned tab: a Live badge + "Open console"
 * when a session is currently live, else a "Start net" action for a
 * schedulable net — plus "Edit" and "Share link", always available.
 *
 * `net.liveSessionId` / `net.nextOccurrenceAt` are backend-gap fields (see
 * `netsApi.ts`'s `OwnedNet` doc comment): they render honestly as
 * "not live" / no next-time facet until the server actually populates them.
 */
function OwnedNetCard({
  net,
  starting,
  startProblem,
  onStart,
  onShareLink,
}: {
  net: OwnedNet;
  starting: boolean;
  startProblem?: Problem;
  onStart: () => void;
  onShareLink: () => void;
}): ReactElement {
  const live = net.liveSessionId !== null;
  // `mode` is an RF connection's property, and `connectionSummary` carries it
  // for the connection that actually has one — the flat column is a mirror
  // that goes stale for an internet-only net.
  const restFacets = [net.netCategory, net.netType]
    .filter((f) => f !== "")
    .join(" · ");

  return (
    <div data-testid="owned-net-card" style={ownedRowStyle(live)}>
        <div style={ownedCardHeadStyle}>
          {/* Same basis as `MyNetRow`'s cell, for the same reason: without it
              the description's max-content width wraps the action row below the
              card head. */}
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={ownedTitleRowStyle}>
              <span style={ownedTitleStyle}>{net.title}</span>
              {live && (
                <span style={liveBadgeStyle}>
                  <span aria-hidden="true" style={liveDotStyle} />
                  Live
                </span>
              )}
            </div>
            <div
              data-testid="owned-net-meta"
              style={{ ...metaStyle, marginTop: "var(--space-1)" }}
            >
              {/* From the CONNECTION SET, never the flat mirror: a net with no
                  RF connection leaves those columns holding whatever they last
                  did, and this line would state the stale number as fact. */}
              <span style={monoFreqStyle}>{connectionSummary(net.connections)}</span>
              {restFacets !== "" && <> · {restFacets}</>}
              {net.nextOccurrenceAt !== null && (
                <>
                  {" · next "}
                  <time dateTime={net.nextOccurrenceAt} style={monoTimeStyle}>
                    {humanizeTime(net.nextOccurrenceAt, new Date())}
                  </time>
                </>
              )}
            </div>
            <DescriptionPreview description={net.description} />
          </div>
          <div style={ownedActionsRowStyle}>
            {live ? (
              <a href={`/net-sessions/${net.liveSessionId}`} style={primaryButtonStyle}>
                Open console
              </a>
            ) : (
              <button type="button" onClick={onStart} disabled={starting} style={outlineButtonStyle}>
                Start net
              </button>
            )}
            <a href={`/nets/${net.id}/edit`} style={neutralButtonStyle}>
              Edit
            </a>
            <button
              type="button"
              aria-label="Share link"
              onClick={onShareLink}
              style={iconButtonStyle}
            >
              <ShareIcon />
            </button>
          </div>
        </div>
        {startProblem !== undefined && (
          <p role="alert" style={errorStyle}>
            {messageForProblem(startProblem)}
          </p>
        )}
    </div>
  );
}

/**
 * The paging lifecycle one tab owns: the first page's
 * loading → success/error, then "Load more" appending pages until `nextCursor`
 * is `null`. Local to this file because both tabs need it and nothing else does
 * — `ProfilePage` and the admin tabs carry their own copies, and migrating those
 * is a separate change. Kept unexported so the file still exports only
 * components.
 *
 * `fetchPage` is a module-level fetcher and therefore a stable reference; the
 * first-page loader depends on it.
 */
function usePagedList<T>(fetchPage: (cursor?: string) => Promise<ListPage<T>>): {
  state: ListState<T>;
  load: () => void;
  loadMore: () => Promise<void>;
  pageLoading: boolean;
  pagingProblem: Problem | undefined;
  removeRows: (keep: (row: T) => boolean) => void;
} {
  const [state, setState] = useState<ListState<T>>({ status: "loading" });
  const [pageLoading, setPageLoading] = useState(false);
  const [pagingProblem, setPagingProblem] = useState<Problem | undefined>(undefined);

  const load = useCallback(() => {
    setState({ status: "loading" });
    setPagingProblem(undefined);
    fetchPage()
      .then((page) =>
        setState({ status: "success", rows: page.items, nextCursor: page.nextCursor }),
      )
      .catch((error: unknown) =>
        setState({
          status: "error",
          problem: error instanceof ProblemError ? error.problem : undefined,
        }),
      );
  }, [fetchPage]);

  const loadMore = async (): Promise<void> => {
    if (state.status !== "success" || state.nextCursor === null) {
      return;
    }
    const { nextCursor } = state;
    setPagingProblem(undefined);
    setPageLoading(true);
    try {
      const page = await fetchPage(nextCursor);
      // A failed page leaves the rows already on screen in place; only a
      // successful one appends. It appends onto the rows as they are NOW, not
      // as they were before the round trip: an unfavorite confirmed while the
      // page was in flight has already removed its row, and a snapshot taken
      // before the await would put that row back.
      setState((prev) =>
        prev.status === "success"
          ? {
              status: "success",
              rows: [...prev.rows, ...page.items],
              nextCursor: page.nextCursor,
            }
          : prev,
      );
    } catch (error: unknown) {
      setPagingProblem(error instanceof ProblemError ? error.problem : undefined);
    } finally {
      setPageLoading(false);
    }
  };

  const removeRows = (keep: (row: T) => boolean): void => {
    setState((prev) =>
      prev.status === "success" ? { ...prev, rows: prev.rows.filter(keep) } : prev,
    );
  };

  return { state, load, loadMore, pageLoading, pagingProblem, removeRows };
}

/**
 * The row under a tab's list: "Load more" while a cursor remains, with a paging
 * problem shown beside it. The `disabled` binding is the SOLE double-submit
 * protection (ruling — no early-return guard, no latch), so a flag
 * the `finally` never lowered would leave it stuck; the tests pin both halves.
 */
function LoadMoreFooter({
  nextCursor,
  pageLoading,
  pagingProblem,
  onLoadMore,
}: {
  nextCursor: string | null;
  pageLoading: boolean;
  pagingProblem: Problem | undefined;
  onLoadMore: () => void;
}): ReactElement | null {
  if (pagingProblem === undefined && nextCursor === null) {
    return null;
  }
  return (
    <div style={tabMessageStyle}>
      {pagingProblem !== undefined && (
        <p role="alert" style={{ ...errorStyle, marginTop: 0, marginBottom: "var(--space-2)" }}>
          {messageForProblem(pagingProblem)}
        </p>
      )}
      {nextCursor !== null && (
        <button
          type="button"
          onClick={onLoadMore}
          disabled={pageLoading}
          style={{ ...secondaryButtonStyle, marginTop: 0 }}
        >
          Load more
        </button>
      )}
    </div>
  );
}

interface TabDef {
  key: TabKey;
  label: string;
  /** The rows loaded so far, or `undefined` before the first page lands. */
  count: number | undefined;
  /** Whether a further page exists — the badge reads `N+` rather than claiming
   * a total it does not have. */
  more: boolean;
}

/**
 * "My Nets": the signed-in account's OWNED nets and
 * FAVORITED nets, as two accessible tabs. Self-gated (there is no
 * router-level auth guard): a signed-out visitor is redirected to
 * `/sign-in`. The Favorites tab surfaces Unlisted AND archived favorites
 * (each with an archived indicator and a `/nets/t/{token}` return link); it
 * never exposes owner ids. Each tab's fetch lifecycle is independent:
 * loading → success/empty → error+retry with NO auto-refresh.
 *
 * The Owned tab's live-status badge and "Open console" action depend on
 * `liveSessionId` on each `OwnedNet` — a field the backend does not populate
 * yet (see `netsApi.ts`). Until it does, every owned net renders as
 * not-live/schedulable, which is the honest behavior for the data actually
 * available today.
 */
export function MyNetsPage(): ReactElement {
  const navigate = useNavigate();
  // undefined = still loading /me; null = signed out; Account = signed in.
  const [account, setAccount] = useState<Account | null | undefined>(undefined);
  const [activeTab, setActiveTab] = useState<TabKey>("owned");
  const owned = usePagedList<OwnedNet>(getOwnedNets);
  const favorites = usePagedList<FavoriteNet>(getMyNets);
  const [startingId, setStartingId] = useState<string | null>(null);
  const [startProblem, setStartProblem] = useState<{ id: string; problem?: Problem } | null>(
    null,
  );
  const now = new Date();
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);

  useEffect(() => {
    let cancelled = false;
    fetchCurrentAccount()
      .then((result) => {
        if (!cancelled) {
          setAccount(result);
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
    if (account === null) {
      void navigate("/sign-in", { replace: true });
    }
  }, [account, navigate]);

  const loadOwned = owned.load;
  const loadFavorites = favorites.load;

  useEffect(() => {
    if (account !== undefined && account !== null) {
      loadOwned();
      loadFavorites();
    }
  }, [account, loadOwned, loadFavorites]);

  const onUnfavorite = async (id: string): Promise<void> => {
    await unfavoriteNet(id);
    // Reflect only after the server confirms: drop the row from the loaded
    // list. Under keyset paging this is CORRECT, not merely tolerable — the
    // cursor is the last loaded row's (favoritedAt, id) position, so removing a
    // row shifts nothing the next page depends on. (Offset paging would have
    // skipped a row here.)
    favorites.removeRows((f) => f.id !== id);
  };

  const onStartNet = async (net: OwnedNet): Promise<void> => {
    setStartingId(net.id);
    setStartProblem(null);
    try {
      // Starting takes no frequency — the session freezes the net's
      // whole connection list, and an internet-only net has none to send.
      const session = await startSession({ definitionId: net.id });
      void navigate(`/net-sessions/${session.id}`);
    } catch (error: unknown) {
      setStartProblem({
        id: net.id,
        problem: error instanceof ProblemError ? error.problem : undefined,
      });
    } finally {
      setStartingId(null);
    }
  };

  const onShareLink = (net: OwnedNet): void => {
    const url = `${window.location.origin}${netPermalink(net.linkToken)}`;
    void navigator.clipboard?.writeText(url);
  };

  if (account === undefined || account === null) {
    // Loading /me or redirecting a signed-out visitor — render nothing rather
    // than flash content.
    return <main style={pageStyle} />;
  }

  const ownedState = owned.state;
  const favoritesState = favorites.state;
  const tabs: TabDef[] = [
    {
      key: "owned",
      label: "Owned",
      count: ownedState.status === "success" ? ownedState.rows.length : undefined,
      more: ownedState.status === "success" && ownedState.nextCursor !== null,
    },
    {
      key: "favorites",
      label: "Favorites",
      count: favoritesState.status === "success" ? favoritesState.rows.length : undefined,
      more: favoritesState.status === "success" && favoritesState.nextCursor !== null,
    },
  ];

  const selectTab = (index: number): void => {
    const tab = tabs[index];
    if (tab === undefined) {
      return;
    }
    setActiveTab(tab.key);
    tabRefs.current[index]?.focus();
  };

  const onTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number): void => {
    if (event.key === "ArrowRight") {
      event.preventDefault();
      selectTab((index + 1) % tabs.length);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      selectTab((index - 1 + tabs.length) % tabs.length);
    } else if (event.key === "Home") {
      event.preventDefault();
      selectTab(0);
    } else if (event.key === "End") {
      event.preventDefault();
      selectTab(tabs.length - 1);
    }
  };

  return (
    <main style={pageStyle}>
      <Breadcrumb items={[{ label: "Nets", href: "/" }, { label: "My Nets" }]} />

      <Panel style={panelStyle}>
        <div style={headerRowStyle} data-page-header>
          <h1 style={headingStyle}>My Nets</h1>
          <a href="/nets/new" style={newNetLinkStyle}>
            <PlusIcon />
            New net
          </a>
        </div>

        <div role="tablist" aria-label="My Nets sections" style={tabListStyle}>
          {tabs.map((tab, index) => (
            <button
              key={tab.key}
              ref={(el) => {
                tabRefs.current[index] = el;
              }}
              type="button"
              role="tab"
              id={`mynets-tab-${tab.key}`}
              aria-selected={activeTab === tab.key}
              aria-controls={`mynets-panel-${tab.key}`}
              tabIndex={activeTab === tab.key ? 0 : -1}
              onClick={() => selectTab(index)}
              onKeyDown={(event) => onTabKeyDown(event, index)}
              style={tabButtonStyle(activeTab === tab.key)}
            >
              {tab.label}
              {tab.count !== undefined && (
                <span style={tabCountStyle}>
                  {tab.count}
                  {tab.more && "+"}
                </span>
              )}
            </button>
          ))}
        </div>

        <div
          role="tabpanel"
          id="mynets-panel-owned"
          aria-labelledby="mynets-tab-owned"
          hidden={activeTab !== "owned"}
          style={tabPanelStyle}
        >
          {activeTab === "owned" && (
            <>
              {ownedState.status === "loading" && (
                <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
                  Loading your owned nets…
                </p>
              )}

              {ownedState.status === "error" && (
                <div style={tabMessageStyle}>
                  <p role="alert" style={errorStyle}>
                    {messageForProblem(ownedState.problem)}
                  </p>
                  <button type="button" onClick={loadOwned} style={secondaryButtonStyle}>
                    Try again
                  </button>
                </div>
              )}

              {ownedState.status === "success" &&
                (isExhaustedEmpty(ownedState) ? (
                  <p data-testid="owned-empty" style={tabMessageStyle}>
                    You don&apos;t own any nets yet.{" "}
                    <a href="/nets/new" style={{ color: "var(--accent-ink)" }}>
                      Create one
                    </a>{" "}
                    to get started.
                  </p>
                ) : (
                  <>
                    {ownedState.rows.length > 0 && (
                      <div style={ownedListStyle}>
                        {ownedState.rows.map((net) => (
                          <OwnedNetCard
                            key={net.id}
                            net={net}
                            starting={startingId === net.id}
                            startProblem={
                              startProblem?.id === net.id ? startProblem.problem : undefined
                            }
                            onStart={() => void onStartNet(net)}
                            onShareLink={() => onShareLink(net)}
                          />
                        ))}
                      </div>
                    )}
                    <LoadMoreFooter
                      nextCursor={ownedState.nextCursor}
                      pageLoading={owned.pageLoading}
                      pagingProblem={owned.pagingProblem}
                      onLoadMore={() => void owned.loadMore()}
                    />
                  </>
                ))}
            </>
          )}
        </div>

        <div
          role="tabpanel"
          id="mynets-panel-favorites"
          aria-labelledby="mynets-tab-favorites"
          hidden={activeTab !== "favorites"}
          style={tabPanelStyle}
        >
          {activeTab === "favorites" && (
            <>
              {favoritesState.status === "loading" && (
                <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
                  Loading your favorites…
                </p>
              )}

              {favoritesState.status === "error" && (
                <div style={tabMessageStyle}>
                  <p role="alert" style={errorStyle}>
                    {messageForProblem(favoritesState.problem)}
                  </p>
                  <button type="button" onClick={loadFavorites} style={secondaryButtonStyle}>
                    Try again
                  </button>
                </div>
              )}

              {favoritesState.status === "success" &&
                (isExhaustedEmpty(favoritesState) ? (
                  <EmptyFavorites />
                ) : (
                  <>
                    {favoritesState.rows.length > 0 && (
                      <ul style={listStyle}>
                        {favoritesState.rows.map((favorite) => (
                          <MyNetRow
                            key={favorite.id}
                            favorite={favorite}
                            now={now}
                            onUnfavorite={onUnfavorite}
                          />
                        ))}
                      </ul>
                    )}
                    <LoadMoreFooter
                      nextCursor={favoritesState.nextCursor}
                      pageLoading={favorites.pageLoading}
                      pagingProblem={favorites.pagingProblem}
                      onLoadMore={() => void favorites.loadMore()}
                    />
                  </>
                ))}
            </>
          )}
        </div>
      </Panel>
    </main>
  );
}
