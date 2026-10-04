// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent, ReactElement } from "react";
import { useNavigate } from "react-router";

import { Breadcrumb } from "../../ui/components/Breadcrumb";
import { Panel } from "../../ui/components/Panel";
import { tokens } from "../../ui/tokens/tokens";
import {
  messageForProblem,
  messageForProblemType,
} from "../../errors/problemMessages";
import { fetchCurrentAccount } from "../auth/authApi";
import type { Account, Problem } from "../auth/authApi";
import { adminDestination } from "./adminGate";
import {
  disableAccount,
  fetchAbuseReports,
  fetchAuditLog,
  reenableAccount,
  resolveAbuseReport,
  searchObjects,
} from "./adminApi";
import type {
  AbuseReport,
  AdminObjectType,
  AdminSearchHit,
  AuditEntry,
  AuditFilters,
} from "./adminApi";

type TabKey = "reports" | "search" | "audit";

/** Short badges for each searchable object kind. */
const OBJECT_LABELS: Record<AdminObjectType, string> = {
  account: "Account",
  "net-definition": "Net",
  "net-session": "Session",
  "abuse-report": "Report",
};

/** The closed audit vocabulary, mirroring the Rust `AuditAction::EVERY` ∪
 * `AdminCapability::{EVERY, RETIRED}`. Retired verbs are listed because rows
 * bearing them are still in the log and must stay filterable. */
const AUDIT_ACTIONS = [
  "signed-in",
  "signed-out",
  "account-self-deleted",
  "role-granted",
  "role-revoked",
  "net-created",
  "net-updated",
  "net-archived",
  "session-started",
  "session-closed",
  "view-reports",
  "resolve-report",
  "disable-account",
  "reenable-account",
  "view-audit-log",
  "search-objects",
  "lookup-account",
] as const;

/** A paginated list load: the accumulated rows plus where the next page starts. */
type ListState<T> =
  | { status: "loading" }
  | { status: "success"; rows: T[]; nextCursor: string | null }
  | { status: "error"; problem?: Problem };

type SearchState =
  | { status: "idle" }
  | { status: "loading" }
  | {
      status: "success";
      hits: AdminSearchHit[];
      /** Object types the server cut at its per-type cap. Empty when complete. */
      truncatedTypes: AdminObjectType[];
    }
  | { status: "error"; problem?: Problem };

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

/** Padding for a tab panel's non-row content (loading/error/empty states). */
const tabMessageStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-row-x)",
};

const rowStyle: CSSProperties = {
  padding: "var(--space-4) var(--space-row-x)",
  borderBottom: "1px solid var(--border)",
  display: "flex",
  gap: "var(--space-4)",
  justifyContent: "space-between",
  alignItems: "flex-start",
  flexWrap: "wrap",
};

const metaStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
};

const bodyTextStyle: CSSProperties = {
  margin: "var(--space-1) 0 0",
  // Reports are arbitrary user text; keep a long unbroken run from widening
  // the row past the page measure.
  overflowWrap: "anywhere",
};

const labelStyle: CSSProperties = {
  display: "block",
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
  marginBottom: "var(--space-1)",
};

const inputStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-3)",
  background: "var(--surface)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  minWidth: "16rem",
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

const secondaryButtonStyle: CSSProperties = {
  padding: "var(--space-2) var(--space-3)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const dangerButtonStyle: CSSProperties = {
  ...secondaryButtonStyle,
  color: "var(--warn)",
  borderColor: "var(--warn)",
  fontWeight: 700,
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
  margin: 0,
};

const searchRowStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-3)",
  alignItems: "flex-end",
  flexWrap: "wrap",
  padding: "var(--space-4) var(--space-row-x)",
};

const tableStyle: CSSProperties = {
  width: "100%",
  borderCollapse: "collapse",
  fontSize: tokens.typography.meta.fontSize,
};

const cellStyle: CSSProperties = {
  textAlign: "left",
  padding: "var(--space-2) var(--space-3)",
  borderBottom: "1px solid var(--border)",
  verticalAlign: "top",
  // Account ids are long opaque strings; let them wrap rather than force the
  // table wider than the page.
  overflowWrap: "anywhere",
};

const monoCellStyle: CSSProperties = {
  ...cellStyle,
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
};

/** The audit table can outgrow narrow viewports; scroll it, not the page. */
const tableScrollStyle: CSSProperties = {
  overflowX: "auto",
  padding: "0 var(--space-row-x) var(--space-4)",
};

const TABS: { key: TabKey; label: string }[] = [
  { key: "reports", label: "Reports" },
  { key: "search", label: "Search" },
  { key: "audit", label: "Audit log" },
];

const badgeStyle: CSSProperties = {
  display: "inline-block",
  marginRight: "var(--space-2)",
  padding: "0 var(--space-2)",
  borderRadius: "var(--rounded-md)",
  background: "var(--surface-2)",
  color: "var(--text-muted)",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
};

const idStyle: CSSProperties = {
  fontFamily: tokens.typography.mono.fontFamily,
  letterSpacing: tokens.typography.mono.letterSpacing,
  overflowWrap: "anywhere",
};

const copyButtonStyle: CSSProperties = {
  padding: "0 var(--space-2)",
  background: "transparent",
  color: "var(--accent-ink)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontSize: tokens.typography.meta.fontSize,
  cursor: "pointer",
};

const filterBarStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-4)",
  alignItems: "flex-end",
  flexWrap: "wrap",
  padding: "var(--space-4) var(--space-row-x)",
  borderBottom: "1px solid var(--border)",
};

const chipStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-2)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontSize: tokens.typography.meta.fontSize,
  cursor: "pointer",
  overflowWrap: "anywhere",
};

/** An id rendered as a button so it can pivot the filter without looking like
 * navigation away from the page. */
const idPivotStyle: CSSProperties = {
  background: "transparent",
  border: "none",
  padding: 0,
  color: "var(--accent-ink)",
  font: "inherit",
  textAlign: "left",
  cursor: "pointer",
  overflowWrap: "anywhere",
};

/**
 * Slugs whose shared copy is written for a different surface than this one.
 *
 * `/errors/forbidden` on an admin surface means "you are not an admin", not the
 * net-ownership refusal the shared map describes. `/errors/validation` reaches
 * this dashboard from a search term, an audit filter, or a page cursor the
 * server did not issue — never from an email address, which is what the shared
 * copy names (made query rejections reachable here). Mapped locally
 * so the shared entries stay correct for the surfaces they were written for.
 */
const ADMIN_MESSAGES: Record<string, string> = {
  "/errors/forbidden": "Your account isn't an administrator on this instance.",
  "/errors/validation":
    "That request wasn't something the server could read — check the search term and filters, then reload and try again.",
};

/**
 * Resolves problem copy for this surface: the server's `detail` first, then
 * this surface's overrides, then the shared map.
 *
 * `detail` beats `ADMIN_MESSAGES`, and that is safe only
 * because of what the two overridden slugs actually carry. `/errors/forbidden`
 * never carries a `detail` at source, so its override is untouched.
 * `/errors/validation`'s `detail` names the
 * offending query key — strictly more useful to an admin than "check the
 * search term and filters". Adding a third override whose slug DOES carry a
 * `detail` would silently disable it; put such copy in `detail` at source
 * instead.
 */
function adminMessage(problem: Problem | undefined): string {
  const slug = problem?.type;
  const shared = messageForProblem(problem);
  // Delegating rather than re-testing `detail` keeps ONE owner of what counts
  // as a usable `detail` (empty string is not) and of the slugs whose `detail`
  // must never be shown raw. The override applies exactly when the shared
  // resolver fell through to the map — i.e. when there was nothing better.
  const fellBackToSharedMap = shared === messageForProblemType(slug);
  if (fellBackToSharedMap && slug !== undefined && slug in ADMIN_MESSAGES) {
    return ADMIN_MESSAGES[slug];
  }
  return shared;
}

/** Renders a timestamp in the viewer's locale, falling back to the raw value. */
function formatTime(iso: string): string {
  const parsed = new Date(iso);
  return Number.isNaN(parsed.getTime()) ? iso : parsed.toLocaleString();
}

/**
 * NetRoll's platform-admin dashboard: the abuse-report review queue, an exact
 * account lookup with disable/re-enable, and the security audit log.
 *
 * Self-gated like every other account-scoped page — a non-admin is sent away
 * rather than shown a surface whose every request the server would refuse. The
 * server's admin gate remains the authority.
 *
 * Reads are explicit: nothing auto-refreshes, and the audit log is fetched only
 * once its tab is opened, because reading it writes an audit row of its own.
 */
export function AdminPage(): ReactElement {
  const navigate = useNavigate();
  // undefined = still loading /me; null = signed out; Account = signed in.
  const [account, setAccount] = useState<Account | null | undefined>(undefined);
  const [activeTab, setActiveTab] = useState<TabKey>("reports");
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);

  const [reportsState, setReportsState] = useState<ListState<AbuseReport>>({
    status: "loading",
  });
  const [resolveProblem, setResolveProblem] = useState<Problem | undefined>();
  const [pagingProblem, setPagingProblem] = useState<Problem | undefined>();
  // "Load more" is not idempotent — a second press re-sends the SAME cursor and
  // appends the same page again — so each tab withdraws its own affordance while
  // a page request is outstanding. The two tabs are separate components with
  // separate state; there is no one flag to share.
  //
  // The withdrawal is `disabled={pageLoading}` on the button, and that binding
  // is the WHOLE mechanism: React dispatches no click to a disabled button,
  // even for a programmatic `dispatchEvent`. The loaders deliberately carry no
  // in-flight guard of their own — one was tried and proven inert, because a
  // loader is redefined per render and closes over that render's flag, so two
  // invocations originating from one render both read `false`. If
  // either loader ever gains a caller that is NOT this disabled-gated button,
  // that caller needs its own protection; the flag below will not supply it.
  const [reportsPageLoading, setReportsPageLoading] = useState(false);
  const [auditState, setAuditState] = useState<ListState<AuditEntry>>({
    status: "loading",
  });
  const [auditPagingProblem, setAuditPagingProblem] = useState<
    Problem | undefined
  >();
  const [auditPageLoading, setAuditPageLoading] = useState(false);
  const [auditOpened, setAuditOpened] = useState(false);

  const [term, setTerm] = useState("");
  const [lastTerm, setLastTerm] = useState("");
  const [searchState, setSearchState] = useState<SearchState>({
    status: "idle",
  });
  const [auditFilters, setAuditFilters] = useState<AuditFilters>({});
  const [pendingDisable, setPendingDisable] = useState<string | null>(null);
  const [actionProblem, setActionProblem] = useState<Problem | undefined>();

  useEffect(() => {
    let cancelled = false;
    fetchCurrentAccount()
      .then((current) => {
        if (!cancelled) {
          setAccount(current);
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

  const destination = adminDestination(account);
  useEffect(() => {
    if (destination !== null) {
      void navigate(destination, { replace: true });
    }
  }, [destination, navigate]);

  const isAdmin = account != null && account.isAdmin;

  const loadReports = useCallback(async (): Promise<void> => {
    setReportsState({ status: "loading" });
    setResolveProblem(undefined);
    setPagingProblem(undefined);
    try {
      const page = await fetchAbuseReports();
      setReportsState({
        status: "success",
        rows: page.items,
        nextCursor: page.nextCursor,
      });
    } catch (error) {
      setReportsState({ status: "error", problem: problemOf(error) });
    }
  }, []);

  // Admin reads wait for the session check: firing them while /me is in flight
  // would 403-spam the server for anyone who lands on the URL.
  useEffect(() => {
    if (isAdmin) {
      void loadReports();
    }
  }, [isAdmin, loadReports]);

  const loadAudit = useCallback(async (): Promise<void> => {
    setAuditState({ status: "loading" });
    setAuditPagingProblem(undefined);
    try {
      const page = await fetchAuditLog(auditFilters);
      setAuditState({
        status: "success",
        rows: page.items,
        nextCursor: page.nextCursor,
      });
    } catch (error) {
      setAuditState({ status: "error", problem: problemOf(error) });
    }
  }, [auditFilters]);

  // Refetches from page 1 whenever the filters change: a cursor is bound to the
  // filter set that issued it, so carrying one across a change would be refused.
  useEffect(() => {
    if (isAdmin && auditOpened) {
      void loadAudit();
    }
  }, [isAdmin, auditOpened, loadAudit]);

  // A failed NEXT page must not discard the pages already on screen — the rows
  // are still valid, and the cursor is still resumable, so the failure is
  // surfaced beside the list rather than replacing it.
  const loadMoreReports = async (): Promise<void> => {
    if (reportsState.status !== "success" || reportsState.nextCursor === null) {
      return;
    }
    const { rows, nextCursor } = reportsState;
    setPagingProblem(undefined);
    setReportsPageLoading(true);
    try {
      const page = await fetchAbuseReports(nextCursor);
      setReportsState({
        status: "success",
        rows: [...rows, ...page.items],
        nextCursor: page.nextCursor,
      });
    } catch (error) {
      setPagingProblem(problemOf(error));
    } finally {
      // `finally`, not the success path: a failed page must leave the button
      // pressable, or one error disables Load-more for the rest of the session.
      setReportsPageLoading(false);
    }
  };

  const loadMoreAudit = async (): Promise<void> => {
    if (auditState.status !== "success" || auditState.nextCursor === null) {
      return;
    }
    const { rows, nextCursor } = auditState;
    setAuditPagingProblem(undefined);
    setAuditPageLoading(true);
    try {
      const page = await fetchAuditLog(auditFilters, nextCursor);
      setAuditState({
        status: "success",
        rows: [...rows, ...page.items],
        nextCursor: page.nextCursor,
      });
    } catch (error) {
      setAuditPagingProblem(problemOf(error));
    } finally {
      setAuditPageLoading(false);
    }
  };

  const resolve = async (id: string): Promise<void> => {
    setResolveProblem(undefined);
    try {
      await resolveAbuseReport(id);
    } catch (error) {
      // The report is still open server-side; dropping the row here would hide
      // it from the queue for good.
      setResolveProblem(problemOf(error));
      return;
    }
    setReportsState((previous) =>
      previous.status === "success"
        ? { ...previous, rows: previous.rows.filter((r) => r.id !== id) }
        : previous,
    );
  };

  const runSearch = useCallback(async (query: string): Promise<void> => {
    setSearchState({ status: "loading" });
    setPendingDisable(null);
    setActionProblem(undefined);
    try {
      const result = await searchObjects(query);
      setSearchState({
        status: "success",
        hits: result.hits,
        truncatedTypes: result.truncatedTypes,
      });
    } catch (error) {
      setSearchState({ status: "error", problem: problemOf(error) });
    }
  }, []);

  /** Applies an audit filter and shows the tab that renders it. */
  const pivot = (filters: AuditFilters): void => {
    setAuditFilters(filters);
    setAuditOpened(true);
    setActiveTab("audit");
  };

  const search = (): void => {
    const query = term.trim();
    if (query === "") {
      return;
    }
    setLastTerm(query);
    void runSearch(query);
  };

  const applyAccountAction = async (
    id: string,
    action: (target: string) => Promise<void>,
  ): Promise<void> => {
    setActionProblem(undefined);
    try {
      await action(id);
    } catch (error) {
      setActionProblem(problemOf(error));
      return;
    }
    setPendingDisable(null);
    // Re-read rather than patching locally: the server owns the disabled state,
    // and a mismatch here would offer the wrong next action.
    if (lastTerm !== "") {
      await runSearch(lastTerm);
    }
  };

  const selectTab = (index: number): void => {
    const tab = TABS[index];
    if (tab === undefined) {
      return;
    }
    setActiveTab(tab.key);
    if (tab.key === "audit") {
      setAuditOpened(true);
    }
    tabRefs.current[index]?.focus();
  };

  const onTabKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    index: number,
  ): void => {
    if (event.key === "ArrowRight") {
      event.preventDefault();
      selectTab((index + 1) % TABS.length);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      selectTab((index - 1 + TABS.length) % TABS.length);
    } else if (event.key === "Home") {
      event.preventDefault();
      selectTab(0);
    } else if (event.key === "End") {
      event.preventDefault();
      selectTab(TABS.length - 1);
    }
  };

  if (!isAdmin) {
    // Loading /me or redirecting a non-admin — render nothing rather than
    // flash a surface the viewer may not keep.
    return <main style={pageStyle} />;
  }

  return (
    <main style={pageStyle}>
      <Breadcrumb items={[{ label: "NetRoll", href: "/" }, { label: "Admin" }]} />

      <Panel style={panelStyle}>
        <div style={headerRowStyle}>
          <h1 style={headingStyle}>Admin</h1>
          <p style={{ ...metaStyle, margin: 0 }}>
            Every action here is recorded in the audit log.
          </p>
        </div>

        <div role="tablist" aria-label="Admin sections" style={tabListStyle}>
          {TABS.map((tab, index) => (
            <button
              key={tab.key}
              ref={(el) => {
                tabRefs.current[index] = el;
              }}
              type="button"
              role="tab"
              id={`admin-tab-${tab.key}`}
              aria-selected={activeTab === tab.key}
              aria-controls={`admin-panel-${tab.key}`}
              tabIndex={activeTab === tab.key ? 0 : -1}
              onClick={() => selectTab(index)}
              onKeyDown={(event) => onTabKeyDown(event, index)}
              style={tabButtonStyle(activeTab === tab.key)}
            >
              {tab.label}
            </button>
          ))}
        </div>

        <div
          role="tabpanel"
          id="admin-panel-reports"
          aria-labelledby="admin-tab-reports"
          hidden={activeTab !== "reports"}
        >
          {activeTab === "reports" && (
            <ReportsTab
              state={reportsState}
              resolveProblem={resolveProblem}
              pagingProblem={pagingProblem}
              pageLoading={reportsPageLoading}
              onRetry={() => void loadReports()}
              onResolve={(id) => void resolve(id)}
              onLoadMore={() => void loadMoreReports()}
            />
          )}
        </div>

        <div
          role="tabpanel"
          id="admin-panel-search"
          aria-labelledby="admin-tab-search"
          hidden={activeTab !== "search"}
        >
          {activeTab === "search" && (
            <SearchTab
              term={term}
              onTermChange={setTerm}
              onSearch={search}
              state={searchState}
              pendingDisable={pendingDisable}
              actionProblem={actionProblem}
              onRequestDisable={setPendingDisable}
              onConfirmDisable={(id) =>
                void applyAccountAction(id, disableAccount)
              }
              onReenable={(id) => void applyAccountAction(id, reenableAccount)}
              onPivot={pivot}
            />
          )}
        </div>

        <div
          role="tabpanel"
          id="admin-panel-audit"
          aria-labelledby="admin-tab-audit"
          hidden={activeTab !== "audit"}
        >
          {activeTab === "audit" && (
            <AuditTab
              state={auditState}
              pagingProblem={auditPagingProblem}
              pageLoading={auditPageLoading}
              filters={auditFilters}
              onFilter={setAuditFilters}
              onRetry={() => void loadAudit()}
              onLoadMore={() => void loadMoreAudit()}
            />
          )}
        </div>
      </Panel>
    </main>
  );
}

/** The parsed problem behind a rejected API call, when it carried one. */
function problemOf(error: unknown): Problem | undefined {
  return typeof error === "object" &&
    error !== null &&
    "problem" in error &&
    typeof error.problem === "object"
    ? (error.problem as Problem)
    : undefined;
}

function ReportsTab({
  state,
  resolveProblem,
  pagingProblem,
  pageLoading,
  onRetry,
  onResolve,
  onLoadMore,
}: {
  state: ListState<AbuseReport>;
  resolveProblem: Problem | undefined;
  pagingProblem: Problem | undefined;
  /** A next-page request is outstanding, so Load more is withdrawn. */
  pageLoading: boolean;
  onRetry: () => void;
  onResolve: (id: string) => void;
  onLoadMore: () => void;
}): ReactElement {
  if (state.status === "loading") {
    return (
      <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
        Loading the report queue…
      </p>
    );
  }
  if (state.status === "error") {
    return (
      <div style={tabMessageStyle}>
        <p role="alert" style={errorStyle}>
          {adminMessage(state.problem)}
        </p>
        <button type="button" onClick={onRetry} style={secondaryButtonStyle}>
          Try again
        </button>
      </div>
    );
  }
  if (state.rows.length === 0) {
    return (
      <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
        No open reports.
      </p>
    );
  }

  return (
    <>
      {resolveProblem !== undefined && (
        <div style={tabMessageStyle}>
          <p role="alert" style={errorStyle}>
            {adminMessage(resolveProblem)}
          </p>
        </div>
      )}
      <ul style={{ listStyle: "none", margin: 0, padding: 0 }}>
        {state.rows.map((report) => (
          <li key={report.id} style={rowStyle}>
            <div style={{ flex: "1 1 24rem" }}>
              <p style={{ ...metaStyle, margin: 0 }}>
                {formatTime(report.createdAt)}
                {report.reporterContact !== null &&
                  ` · from ${report.reporterContact}`}
              </p>
              <p style={bodyTextStyle}>{report.body}</p>
              {report.contextUrl !== null && (
                <p style={{ ...metaStyle, margin: "var(--space-1) 0 0" }}>
                  {report.contextUrl}
                </p>
              )}
            </div>
            <button
              type="button"
              onClick={() => onResolve(report.id)}
              style={secondaryButtonStyle}
            >
              Resolve
            </button>
          </li>
        ))}
      </ul>
      {(pagingProblem !== undefined || state.nextCursor !== null) && (
        <div style={tabMessageStyle}>
          {pagingProblem !== undefined && (
            <p role="alert" style={{ ...errorStyle, marginBottom: "var(--space-2)" }}>
              {adminMessage(pagingProblem)}
            </p>
          )}
          {state.nextCursor !== null && (
            <button
              type="button"
              onClick={onLoadMore}
              disabled={pageLoading}
              style={secondaryButtonStyle}
            >
              Load more
            </button>
          )}
        </div>
      )}
    </>
  );
}

function SearchTab({
  term,
  onTermChange,
  onSearch,
  state,
  pendingDisable,
  actionProblem,
  onRequestDisable,
  onConfirmDisable,
  onReenable,
  onPivot,
}: {
  term: string;
  onTermChange: (value: string) => void;
  onSearch: () => void;
  state: SearchState;
  pendingDisable: string | null;
  actionProblem: Problem | undefined;
  onRequestDisable: (id: string | null) => void;
  onConfirmDisable: (id: string) => void;
  onReenable: (id: string) => void;
  onPivot: (filters: AuditFilters) => void;
}): ReactElement {
  return (
    <>
      {/* Explicit submit, NOT search-as-you-type: every search writes an audit
          row, so a debounced box would make "an admin typed" the most common
          event in the log. */}
      <form
        style={searchRowStyle}
        onSubmit={(event) => {
          event.preventDefault();
          onSearch();
        }}
      >
        <div>
          <label htmlFor="admin-search-q" style={labelStyle}>
            Callsign, name, net title, or ID
          </label>
          <input
            id="admin-search-q"
            type="text"
            autoComplete="off"
            value={term}
            onChange={(event) => onTermChange(event.target.value)}
            style={inputStyle}
          />
        </div>
        <button type="submit" style={primaryButtonStyle}>
          Search
        </button>
      </form>
      <p style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
        Callsigns, names and net titles match partially. Email addresses and IDs
        must be exact.
      </p>

      {state.status === "loading" && (
        <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
          Searching…
        </p>
      )}
      {state.status === "error" && (
        <div style={tabMessageStyle}>
          <p role="alert" style={errorStyle}>
            {adminMessage(state.problem)}
          </p>
        </div>
      )}
      {state.status === "success" && state.hits.length === 0 && (
        <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
          Nothing matches that.
        </p>
      )}
      {/* Rendered ALONGSIDE the hits, not instead of them: the rows shown are
          real, the notice only says they are not all of them. */}
      {state.status === "success" && state.truncatedTypes.length > 0 && (
        <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
          More matched than are shown for{" "}
          {state.truncatedTypes.map((t) => OBJECT_LABELS[t]).join(", ")}.
          Narrow the search to reach the rest.
        </p>
      )}
      {state.status === "success" &&
        state.hits.map((hit) => (
          <div key={`${hit.objectType}:${hit.id}`} style={rowStyle}>
            <div style={{ flex: "1 1 24rem" }}>
              <p style={{ margin: 0 }}>
                <span style={badgeStyle}>{OBJECT_LABELS[hit.objectType]}</span>
                <span style={{ fontWeight: 700 }}>
                  {hit.label ?? "(no name)"}
                </span>
              </p>
              {hit.sublabel !== null && (
                <p style={{ ...metaStyle, margin: "var(--space-1) 0 0" }}>
                  {hit.sublabel}
                </p>
              )}
              <p style={{ ...metaStyle, margin: "var(--space-1) 0 0" }}>
                {hit.disabledAt !== null
                  ? `Disabled ${formatTime(hit.disabledAt)}`
                  : hit.inactiveAt !== null
                    ? `Inactive since ${formatTime(hit.inactiveAt)}`
                    : "Active"}
              </p>
              <IdWithCopy id={hit.id} />
              {actionProblem !== undefined && (
                <p role="alert" style={{ ...errorStyle, marginTop: "var(--space-2)" }}>
                  {adminMessage(actionProblem)}
                </p>
              )}
            </div>
            <div style={{ display: "flex", gap: "var(--space-2)", flexWrap: "wrap" }}>
              {/* The pivots are the point: they carry the id straight into the
                  audit filters so an admin never retypes a uuid. */}
              {hit.objectType === "account" && (
                <button
                  type="button"
                  onClick={() => onPivot({ actor: hit.id })}
                  style={secondaryButtonStyle}
                >
                  Actions by this
                </button>
              )}
              <button
                type="button"
                onClick={() => onPivot({ object: hit.id })}
                style={secondaryButtonStyle}
              >
                Actions on this
              </button>
              {hit.objectType === "account" &&
                (hit.disabledAt !== null ? (
                  <button
                    type="button"
                    onClick={() => onReenable(hit.id)}
                    style={secondaryButtonStyle}
                  >
                    Re-enable
                  </button>
                ) : pendingDisable === hit.id ? (
                  <>
                    <button
                      type="button"
                      onClick={() => onConfirmDisable(hit.id)}
                      style={dangerButtonStyle}
                    >
                      Confirm disable
                    </button>
                    <button
                      type="button"
                      onClick={() => onRequestDisable(null)}
                      style={secondaryButtonStyle}
                    >
                      Cancel
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    onClick={() => onRequestDisable(hit.id)}
                    style={dangerButtonStyle}
                  >
                    Disable
                  </button>
                ))}
            </div>
          </div>
        ))}
    </>
  );
}

/** An object id shown in full, with a one-click copy — the "expose the UUID"
 * affordance. Shown in full rather than truncated: a partial uuid is useless
 * for the filters and endpoints it feeds. */
function IdWithCopy({ id }: { id: string }): ReactElement {
  const [copied, setCopied] = useState(false);
  return (
    <p style={{ ...metaStyle, margin: "var(--space-2) 0 0", display: "flex", gap: "var(--space-2)", alignItems: "center", flexWrap: "wrap" }}>
      <code style={idStyle}>{id}</code>
      <button
        type="button"
        style={copyButtonStyle}
        onClick={() => {
          // `navigator.clipboard` is absent in insecure contexts and in jsdom;
          // the id is on screen and selectable either way, so a failure just
          // means no confirmation, never a broken page.
          void navigator.clipboard?.writeText(id).then(
            () => setCopied(true),
            () => setCopied(false),
          );
        }}
      >
        Copy ID
      </button>
      {copied && (
        <span role="status" style={metaStyle}>
          Copied
        </span>
      )}
    </p>
  );
}

function AuditTab({
  state,
  pagingProblem,
  pageLoading,
  filters,
  onFilter,
  onRetry,
  onLoadMore,
}: {
  state: ListState<AuditEntry>;
  pagingProblem: Problem | undefined;
  /** A next-page request is outstanding, so Load more is withdrawn. */
  pageLoading: boolean;
  filters: AuditFilters;
  onFilter: (next: AuditFilters) => void;
  onRetry: () => void;
  onLoadMore: () => void;
}): ReactElement {
  const chips = ([
    ["actor", "Actor", filters.actor],
    ["object", "Object", filters.object],
    ["action", "Action", filters.action],
  ] as const).filter(([, , value]) => value !== undefined);

  const bar = (
    <div style={filterBarStyle}>
      <div>
        <label htmlFor="admin-audit-action" style={labelStyle}>
          Action
        </label>
        <select
          id="admin-audit-action"
          value={filters.action ?? ""}
          style={inputStyle}
          onChange={(event) =>
            onFilter({
              ...filters,
              action: event.target.value === "" ? undefined : event.target.value,
            })
          }
        >
          <option value="">Any action</option>
          {AUDIT_ACTIONS.map((verb) => (
            <option key={verb} value={verb}>
              {verb}
            </option>
          ))}
        </select>
      </div>
      {chips.length > 0 && (
        <div style={{ display: "flex", gap: "var(--space-2)", flexWrap: "wrap", alignItems: "center" }}>
          {chips.map(([key, label, value]) => (
            <button
              key={key}
              type="button"
              style={chipStyle}
              aria-label={`Clear ${label} filter`}
              onClick={() => onFilter({ ...filters, [key]: undefined })}
            >
              {label}: {value} ×
            </button>
          ))}
          <button type="button" style={secondaryButtonStyle} onClick={() => onFilter({})}>
            Clear all
          </button>
        </div>
      )}
    </div>
  );

  if (state.status === "loading") {
    return (
      <>
        {bar}
        <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
          Loading the audit log…
        </p>
      </>
    );
  }
  if (state.status === "error") {
    return (
      <>
        {bar}
        <div style={tabMessageStyle}>
          <p role="alert" style={errorStyle}>
            {adminMessage(state.problem)}
          </p>
          <button type="button" onClick={onRetry} style={secondaryButtonStyle}>
            Try again
          </button>
        </div>
      </>
    );
  }

  return (
    <>
      {bar}
      <p style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
        Newest first. Opening this tab records a read of its own.
      </p>
      <div style={tableScrollStyle}>
        <table style={tableStyle}>
          <caption style={{ ...metaStyle, textAlign: "left", padding: "var(--space-2) 0" }}>
            Security audit log
          </caption>
          <thead>
            <tr>
              <th scope="col" style={cellStyle}>
                When
              </th>
              <th scope="col" style={cellStyle}>
                Action
              </th>
              <th scope="col" style={cellStyle}>
                Actor
              </th>
              <th scope="col" style={cellStyle}>
                Target
              </th>
              <th scope="col" style={cellStyle}>
                Detail
              </th>
            </tr>
          </thead>
          <tbody>
            {state.rows.map((entry) => (
              <tr key={entry.id}>
                <td style={cellStyle}>{formatTime(entry.occurredAt)}</td>
                <td style={cellStyle}>{entry.action}</td>
                <td style={monoCellStyle}>
                  {/* Clicking an id pivots the filter in place, so an admin
                      never has to copy a uuid back into the search box. */}
                  <button
                    type="button"
                    style={idPivotStyle}
                    onClick={() => onFilter({ actor: entry.actorAccountId })}
                  >
                    {entry.actorAccountId}
                  </button>
                </td>
                <td style={monoCellStyle}>
                  {entry.targetId === null ? (
                    "—"
                  ) : (
                    <button
                      type="button"
                      style={idPivotStyle}
                      onClick={() => onFilter({ object: entry.targetId ?? undefined })}
                    >
                      {entry.targetId}
                    </button>
                  )}
                  {entry.targetType !== null && (
                    <span style={metaStyle}> ({entry.targetType})</span>
                  )}
                </td>
                <td style={cellStyle}>
                  {entry.metadata != null ? JSON.stringify(entry.metadata) : "—"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {state.rows.length === 0 && (
        <p role="status" style={{ ...metaStyle, ...tabMessageStyle, margin: 0 }}>
          No audit records yet.
        </p>
      )}
      {(pagingProblem !== undefined || state.nextCursor !== null) && (
        <div style={tabMessageStyle}>
          {pagingProblem !== undefined && (
            <p role="alert" style={{ ...errorStyle, marginBottom: "var(--space-2)" }}>
              {adminMessage(pagingProblem)}
            </p>
          )}
          {state.nextCursor !== null && (
            <button
              type="button"
              onClick={onLoadMore}
              disabled={pageLoading}
              style={secondaryButtonStyle}
            >
              Load more
            </button>
          )}
        </div>
      )}
    </>
  );
}
