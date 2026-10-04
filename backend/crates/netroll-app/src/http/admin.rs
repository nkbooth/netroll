// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Bounded, audit-logged platform-admin endpoints.
//!
//! Six routes, one capability each, each writing a PII-free `audit_log` append.
//! The append is post-commit and its failure is swallowed and logged: the effect
//! has landed, so a 500 would claim an action did not take when it did.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use netroll_adapters::pg::abuse_reports::ResolveOutcome;
use netroll_adapters::pg::accounts::{DisableOutcome, ReenableOutcome};
use netroll_adapters::pg::admin_search::SearchHit;
use netroll_domain::admin::{
    AdminCapability, FilterFingerprint, PageCursor, clamp_limit, encode_cursor, encode_cursor_for,
    filter_fingerprint, parse_cursor, parse_cursor_for,
};
use netroll_domain::audit::AuditAction;
use netroll_domain::auth::normalize_email;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::middleware::admin::AdminAccount;

use super::audit::{AuditSubject, append_audit};
use super::problem::ApiError;
use super::{AppState, AppStrictQuery, rfc3339};

/// The bounded admin routes, merged into the session-gated protected tree; each
/// handler additionally passes the [`AdminAccount`] gate.
pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/abuse-reports", get(list_abuse_reports))
        .route(
            "/api/admin/abuse-reports/{id}/resolve",
            post(resolve_abuse_report),
        )
        .route("/api/admin/search", get(search_objects))
        .route("/api/admin/accounts/{id}/disable", post(disable_account))
        .route("/api/admin/accounts/{id}/reenable", post(reenable_account))
        .route("/api/admin/audit-log", get(list_audit_log))
}

/// One unresolved report as shown to an admin (camelCase wire).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AbuseReportView {
    id: Uuid,
    created_at: String,
    reporter_contact: Option<String>,
    body: String,
    context_url: Option<String>,
}

/// One audit-log record as shown to an admin (camelCase wire). Reads ONLY
/// the `audit_log` columns — actor / action / target / timestamp / metadata —
/// none of which holds a per-user secret (the caller-discipline convention),
/// so the admin-unreadable invariant holds trivially.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditLogView {
    id: Uuid,
    occurred_at: String,
    actor_account_id: Uuid,
    action: String,
    target_type: Option<String>,
    target_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
}

/// The `?limit=&cursor=` params shared by the paged reads.
///
/// Visible to the rest of `http` (rather than private here) so a second paged
/// surface reuses this one instead of copying it: the duplicated part would be
/// the cursor's REFUSAL behaviour — a malformed cursor is an error, never a
/// silent restart — which is the one place two drifting copies would be a real
/// defect rather than cosmetic. Reads with their own FILTERS keep their own
/// query type, and theirs is the stronger guarantee: their cursor is bound to
/// the filter set it was issued under (see [`AuditQuery::cursor`]).
///
/// **Strict:** every consumer of this type is a FILTERED read, so an
/// unrecognised parameter is a 400 here rather than a dropped key. A dropped
/// cursor changes the MEANING of the answer, which is why all four share it.
/// That makes the reuse this doc invites conditional: a future paged read that
/// should stay LENIENT cannot use this type, because `deny_unknown_fields` is
/// all-or-nothing per struct. Give it its own query type sharing
/// [`PageQuery::cursor()`]'s parsing. That is a **compile error** rather than a
/// guarded hope: this type is [`crate::http::StrictQuery`], so
/// `AppQuery<PageQuery>` does not satisfy the lenient extractor's bound at all.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageQuery {
    pub(super) limit: Option<u32>,
    cursor: Option<String>,
}

impl crate::http::QueryKeyPolicy for PageQuery {
    const REASON: &'static str = "the abuse-report queue and check-in history, and the \
         favorites and owned-nets reads on the same forward rule: a dropped cursor \
         silently restarts the page at 1, so the caller reads page 1 as \"the next page\"";
}

impl crate::http::StrictQuery for PageQuery {}

impl PageQuery {
    /// The resume position, or `None` for the first page.
    ///
    /// A MALFORMED cursor is a hard [`ApiError::Validation`] rather than a
    /// silent restart — restarting would turn a client bug into an endless
    /// first page instead of a visible error.
    ///
    /// What this does NOT check is issuance. [`parse_cursor`] verifies
    /// only that the string is `{millis}:{uuid}` naming a representable
    /// instant; nothing binds a cursor to the read that minted it. So a
    /// `nextCursor` from ANY read using this type is accepted by every other
    /// one and silently anchors the list at an unrelated position — a short
    /// list, not an error. That is reachable one line away now that a single
    /// screen holds two of these cursors at once (the "My Nets" tabs). Reads
    /// that need the stronger property use [`parse_cursor_for`] with a
    /// [`FilterFingerprint`], as the audit log does; these reads deliberately
    /// do not, because their result is always scoped to the caller's own
    /// account and a crossed cursor costs the caller rows, never anyone else's.
    pub(super) fn cursor(&self) -> Result<Option<PageCursor>, ApiError> {
        self.cursor
            .as_deref()
            .map(parse_cursor)
            .transpose()
            // The wire `detail` is left exactly as it shipped, though it says
            // "issued" and issuance is not what is checked. It is a pinned
            // contract (`api_admin.rs` asserts this string deliberately, the
            // one place the no-prose rule is inverted), so correcting it is a
            // wire change and a separate decision — not something a comment
            // pass gets to make. The docstring above says what is true.
            .map_err(|_| ApiError::validation("page cursor is not one this server issued"))
    }
}

/// The audit log's investigation filters, on top of `?limit=&cursor=`.
///
/// `actor` answers "what did this account do?"; `object` answers "what touched
/// this thing?" for an account, report, session, or net.
///
/// **`deny_unknown_fields` is load-bearing.** Without it a typo'd
/// `?actorr=<uuid>` was dropped and this read answered 200 with the UNFILTERED
/// log — the outcome [`AuditQuery::uuid`] below already calls "the worst
/// possible default on this surface". It is the same argument one level up from
/// [`AuditQuery::action`]'s closed-vocabulary refusal: an unknown *value* and an
/// unknown *key* both produce an answer that will be believed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditQuery {
    limit: Option<u32>,
    cursor: Option<String>,
    actor: Option<String>,
    action: Option<String>,
    /// Any object id — an account, abuse report, net session, or net definition.
    object: Option<String>,
}

impl crate::http::QueryKeyPolicy for AuditQuery {
    const REASON: &'static str = "the audit log. A dropped filter returns the \
         UNFILTERED log, which AuditQuery::uuid's own doc calls the worst possible default on this \
         surface";
}

impl crate::http::StrictQuery for AuditQuery {}

impl AuditQuery {
    /// Parses a uuid filter, refusing garbage rather than ignoring it.
    ///
    /// A silently-dropped filter returns the UNFILTERED log, which reads as "no
    /// restriction applied" — the worst possible default on this surface.
    fn uuid(raw: Option<&String>, what: &'static str) -> Result<Option<Uuid>, ApiError> {
        raw.map(|v| Uuid::parse_str(v).map_err(|_| ApiError::validation(what)))
            .transpose()
    }

    fn actor(&self) -> Result<Option<Uuid>, ApiError> {
        Self::uuid(self.actor.as_ref(), "actor filter is not a valid id")
    }

    fn object(&self) -> Result<Option<Uuid>, ApiError> {
        Self::uuid(self.object.as_ref(), "object filter is not a valid id")
    }

    /// The action filter, validated against the CLOSED audit vocabulary.
    ///
    /// An unknown verb is a 400, not an empty page: a silently-empty result is
    /// indistinguishable from "this actually never happened", which on an audit
    /// surface is the answer most likely to be believed and most likely wrong.
    /// Retired verbs stay accepted — rows bearing them are still in the log and
    /// must remain reachable.
    fn action(&self) -> Result<Option<&str>, ApiError> {
        match self.action.as_deref() {
            None => Ok(None),
            Some(verb) if is_known_audit_action(verb) => Ok(Some(verb)),
            Some(_) => Err(ApiError::validation("unknown audit action")),
        }
    }

    /// Stamps the active filter set so a cursor cannot be replayed under a
    /// different one.
    fn fingerprint(&self) -> Result<FilterFingerprint, ApiError> {
        Ok(filter_fingerprint(
            self.actor()?,
            self.action()?,
            self.object()?,
        ))
    }

    /// The resume position, refused unless issued under these same filters.
    fn cursor(&self) -> Result<Option<PageCursor>, ApiError> {
        let fp = self.fingerprint()?;
        self.cursor
            .as_deref()
            .map(|c| parse_cursor_for(c, fp))
            .transpose()
            .map_err(|_| ApiError::validation("page cursor was issued for a different filter set"))
    }
}

/// Whether `verb` is a verb this log can contain — the live admin capabilities,
/// the retired ones (whose rows still exist), and the consolidated audit
/// actions.
fn is_known_audit_action(verb: &str) -> bool {
    AdminCapability::EVERY
        .iter()
        .chain(AdminCapability::RETIRED.iter())
        .any(|c| c.as_str() == verb)
        || AuditAction::EVERY.iter().any(|a| a.as_str() == verb)
}

/// One page of the abuse-report queue (camelCase wire). `nextCursor` is
/// `null` on the last page.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AbuseReportPageBody {
    items: Vec<AbuseReportView>,
    next_cursor: Option<String>,
}

/// One page of the audit log (camelCase wire). `nextCursor` is `null` on
/// the last page.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditLogPageBody {
    items: Vec<AuditLogView>,
    next_cursor: Option<String>,
}

/// Upper bound on a search term, mirroring discovery's `MAX_NAME_FILTER_CHARS`.
const MAX_SEARCH_TERM_CHARS: usize = 120;

/// Rows SERVED per object type. Per-type rather than one shared budget: a
/// single cap would let a term matching many nets crowd accounts out of the
/// results, which an admin reads as "no such account" — a wrong answer.
///
/// `pub` so the integration test seeds against the constant rather than
/// against a copy of its value, which is expected to move.
pub const PER_TYPE_SEARCH_LIMIT: usize = 20;

/// Rows REQUESTED per object type: one more than is served, so a full result
/// can be told apart from an exhausted one without a second query.
const PER_TYPE_SEARCH_FETCH: i64 = PER_TYPE_SEARCH_LIMIT as i64 + 1;

/// Splits an over-fetched read into the rows to serve and whether more matched.
///
/// Over-fetch rather than `rows.len() == limit`: a type matching EXACTLY the
/// cap is complete, and a length test would report it truncated — trading the
/// wrong answer this read gave before for a different one.
fn split_truncation(mut rows: Vec<SearchHit>, limit: usize) -> (Vec<SearchHit>, bool) {
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    (rows, truncated)
}

/// The `?q=&type=` search params. `q` is optional so a missing param reaches
/// the same SPECIFIC validation refusal as a blank one, rather than the
/// extractor's generic query rejection.
///
/// **Strict:** a dropped `?type=` **widens** the search
/// silently — the admin gets hits from every object type while the surface still
/// shows the filter as applied, so the answer stops meaning what the filter says
/// it means. It returns MORE, not fewer: `search_objects` matches every type
/// when the key is absent, and [`PER_TYPE_SEARCH_LIMIT`] is per type, so nothing
/// is crowded out. Note the field is the raw
/// identifier `r#type`, so its serde name is `type` and the attribute must not
/// start refusing it — pinned by
/// `api_admin.rs::the_search_type_narrowing_param_still_binds_under_strictness`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchQuery {
    q: Option<String>,
    /// Narrows to one object type; absent searches every type.
    r#type: Option<String>,
}

impl crate::http::QueryKeyPolicy for SearchQuery {
    const REASON: &'static str = "admin search: named in the 2026-08-26 ruling. A dropped ?type= WIDENS the search — the \
         caller gets hits from every object type while the surface still shows the filter as \
         applied, so the answer stops meaning what the filter says it means. A dropped ?q= reaches \
         the specific blank-term refusal, which is fine; the KEY is what changes the meaning";
}

impl crate::http::StrictQuery for SearchQuery {}

/// One matched object (camelCase wire).
///
/// Identity fields ONLY — what an admin needs to recognise the object and pivot
/// from its id. No per-user secret appears here and none is read to build it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchHitView {
    /// `account` | `net-definition` | `net-session` | `abuse-report`.
    object_type: &'static str,
    id: Uuid,
    /// Callsign, net title, or `null` when the object has no human name.
    label: Option<String>,
    /// Email, lifecycle, or `null`.
    sublabel: Option<String>,
    /// When an admin disabled this account; accounts only. Picks Disable vs
    /// Re-enable in the dashboard.
    disabled_at: Option<String>,
    /// When this object was archived, closed, resolved, or self-deleted.
    inactive_at: Option<String>,
}

impl SearchHitView {
    fn new(object_type: &'static str, hit: SearchHit) -> Self {
        Self {
            object_type,
            id: hit.id,
            label: hit.label,
            sublabel: hit.sublabel,
            disabled_at: hit.disabled_at_millis.map(rfc3339),
            inactive_at: hit.inactive_at_millis.map(rfc3339),
        }
    }
}

/// The search envelope, matching the app's camelCase list convention.
///
/// `truncatedTypes` names every searched object type for which MORE rows
/// matched than the [`PER_TYPE_SEARCH_LIMIT`] served — a type matching exactly
/// the cap is complete and is not named. It is ALWAYS present — an empty array
/// is the complete case — so a caller never has to tell "nothing was cut" from
/// "this server does not say". A type that was not searched never appears.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchBody {
    items: Vec<SearchHitView>,
    truncated_types: Vec<&'static str>,
}

/// Appends one admin action to the audit log via the shared post-commit seam
/// ([`append_audit`], DRY refactor): the capability's stable wire
/// spelling is the `action`; metadata is optional, bounded, and NEVER carries
/// PII/secrets. Swallow-and-log posture lives in `append_audit` — see its doc.
async fn audit(
    state: &AppState,
    actor: Uuid,
    capability: AdminCapability,
    subject: AuditSubject,
    metadata: Option<serde_json::Value>,
    now: u64,
) {
    append_audit(state, actor, capability.as_str(), subject, metadata, now).await;
}

/// `GET /api/admin/abuse-reports?limit=&cursor=` →
/// [`AdminCapability::ViewReports`]. One page of the unresolved review queue,
/// oldest first, plus the cursor for the next page — and records the view in
/// the audit log.
///
/// Paged so a large backlog does not serialize the whole table on every
/// dashboard view. The cursor is opaque: clients echo back `nextCursor`, they
/// never construct one.
async fn list_abuse_reports(
    State(state): State<AppState>,
    AdminAccount(current): AdminAccount,
    AppStrictQuery(params): AppStrictQuery<PageQuery>,
) -> Result<Json<AbuseReportPageBody>, ApiError> {
    let page = state
        .abuse_reports
        .list_unresolved_page(clamp_limit(params.limit), params.cursor()?)
        .await?;
    let now = state.clock.now_epoch_millis();
    audit(
        &state,
        current.account_id,
        AdminCapability::ViewReports,
        AuditSubject::NONE,
        None,
        now,
    )
    .await;
    tracing::info!(account_id = %current.account_id, "admin viewed abuse reports");

    let items = page
        .rows
        .into_iter()
        .map(|r| AbuseReportView {
            id: r.id,
            created_at: rfc3339(r.created_at_millis),
            reporter_contact: r.reporter_contact,
            body: r.body,
            context_url: r.context_url,
        })
        .collect();
    Ok(Json(AbuseReportPageBody {
        items,
        next_cursor: page.next.map(encode_cursor),
    }))
}

/// `POST /api/admin/abuse-reports/{id}/resolve` → [`AdminCapability::ResolveReport`].
async fn resolve_abuse_report(
    State(state): State<AppState>,
    AdminAccount(current): AdminAccount,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let now = state.clock.now_epoch_millis();
    let outcome = match state
        .abuse_reports
        .resolve(id, current.account_id, now)
        .await
    {
        Ok(outcome) => outcome,
        Err(sqlx::Error::RowNotFound) => return Err(ApiError::AbuseReportNotFound),
        Err(err) => return Err(err.into()),
    };
    let resolved = matches!(outcome, ResolveOutcome::Resolved);
    audit(
        &state,
        current.account_id,
        AdminCapability::ResolveReport,
        AuditSubject::target("abuse-report", Some(id)),
        Some(serde_json::json!({ "newlyResolved": resolved })),
        now,
    )
    .await;
    tracing::info!(account_id = %current.account_id, report_id = %id, "admin resolved abuse report");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/admin/search?q=[&type=]` → [`AdminCapability::SearchObjects`].
///
/// Resolves business objects by identifier or name so an admin can turn "this
/// operator" or "this net" into the id every action endpoint and audit filter
/// requires. An abuse report is free text with no target FK, so without this
/// there is no path from a report to anything actionable.
///
/// MATCHING, and why it is asymmetric: callsign and display name match by
/// PREFIX and net/session titles by SUBSTRING — all public radio data, already
/// shown on public net pages. **Email and every id match EXACTLY.** That keeps
/// this a targeting tool for someone who already holds an identifier rather
/// than an address-harvesting sweep.
///
/// A bare uuid probes every type, so a value copied out of the audit log
/// resolves back to a named object. Abuse reports are reachable ONLY that way —
/// a report body is a reporter's free text and is never search-matched.
///
/// Results are capped PER TYPE: one shared budget would let a term matching
/// many nets crowd accounts out entirely, which reads as "no such account".
/// A blank term is [`ApiError::Validation`], never an unbounded listing.
async fn search_objects(
    State(state): State<AppState>,
    AdminAccount(current): AdminAccount,
    AppStrictQuery(params): AppStrictQuery<SearchQuery>,
) -> Result<Json<SearchBody>, ApiError> {
    let term = params.q.as_deref().unwrap_or_default().trim();
    if term.is_empty() {
        return Err(ApiError::validation("a search term is required"));
    }
    if term.chars().count() > MAX_SEARCH_TERM_CHARS {
        return Err(ApiError::validation("search term is too long"));
    }

    let mut items: Vec<SearchHitView> = Vec::new();
    let mut truncated_types: Vec<&'static str> = Vec::new();

    // A bare id is unambiguous — resolve it and stop, rather than also running
    // every name match against a uuid-shaped string. It answers at most one
    // row, so it can never be truncated and is never split.
    if let Ok(id) = Uuid::parse_str(term) {
        if let Some((object_type, hit)) = state.admin_search.by_id(id).await? {
            items.push(SearchHitView::new(object_type, hit));
        }
    } else {
        let wants = |t: &str| params.r#type.as_deref().is_none_or(|want| want == t);
        if wants("account") {
            // An address must be known in FULL to be found; only a callsign or
            // display name may be matched by prefix. `@` is the discriminator,
            // and an address is normalized the same way the account model
            // stores it so casing/whitespace do not defeat the exact match.
            // The exact path answers at most one row, so it is never split.
            if term.contains('@') {
                if let Some(hit) = state
                    .admin_search
                    .account_by_exact_email(&normalize_email(term))
                    .await?
                {
                    items.push(SearchHitView::new("account", hit));
                }
            } else {
                let (rows, truncated) = split_truncation(
                    state
                        .admin_search
                        .accounts_by_prefix(term, PER_TYPE_SEARCH_FETCH)
                        .await?,
                    PER_TYPE_SEARCH_LIMIT,
                );
                items.extend(
                    rows.into_iter()
                        .map(|hit| SearchHitView::new("account", hit)),
                );
                if truncated {
                    truncated_types.push("account");
                }
            }
        }
        if wants("net-definition") {
            let (rows, truncated) = split_truncation(
                state
                    .admin_search
                    .net_definitions_by_title(term, PER_TYPE_SEARCH_FETCH)
                    .await?,
                PER_TYPE_SEARCH_LIMIT,
            );
            items.extend(
                rows.into_iter()
                    .map(|hit| SearchHitView::new("net-definition", hit)),
            );
            if truncated {
                truncated_types.push("net-definition");
            }
        }
        if wants("net-session") {
            let (rows, truncated) = split_truncation(
                state
                    .admin_search
                    .net_sessions_by_snapshot_title(term, PER_TYPE_SEARCH_FETCH)
                    .await?,
                PER_TYPE_SEARCH_LIMIT,
            );
            items.extend(
                rows.into_iter()
                    .map(|hit| SearchHitView::new("net-session", hit)),
            );
            if truncated {
                truncated_types.push("net-session");
            }
        }
    }

    let now = state.clock.now_epoch_millis();
    audit(
        &state,
        current.account_id,
        AdminCapability::SearchObjects,
        AuditSubject::NONE,
        // The term may itself BE an email, and so may a match — only the shape
        // of the outcome is recorded. `shownCount`, not `matchCount`:
        // the reads are capped, so this is what was served, not what exists.
        Some(serde_json::json!({ "shownCount": items.len() })),
        now,
    )
    .await;
    // Actor id only; never the term, never a matched account's email.
    tracing::info!(account_id = %current.account_id, shown = items.len(), "admin searched objects");

    Ok(Json(SearchBody {
        items,
        truncated_types,
    }))
}

/// `POST /api/admin/accounts/{id}/disable` → [`AdminCapability::DisableAccount`].
/// Disables the target account and revokes its live sessions (in the repo's own
/// transaction), then records the action. Reason capture from the request is
/// deferred (the `disabled_reason` column exists for a future story).
///
/// Refuses a SELF-target with [`ApiError::CannotDisableSelf`]: admin status is
/// a boot-configured email allowlist, not a
/// DB-editable role, so a self-disable would revoke the acting admin's own
/// sessions and refuse them from ever signing back in to `reenable`
/// themselves — an unrecoverable lockout on a single-admin instance.
async fn disable_account(
    State(state): State<AppState>,
    AdminAccount(current): AdminAccount,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if id == current.account_id {
        return Err(ApiError::CannotDisableSelf);
    }
    let now = state.clock.now_epoch_millis();
    let outcome = match state.accounts.disable(id, now, None).await {
        Ok(outcome) => outcome,
        Err(sqlx::Error::RowNotFound) => return Err(ApiError::AccountNotFound),
        Err(err) => return Err(err.into()),
    };
    let newly = matches!(outcome, DisableOutcome::Disabled);
    audit(
        &state,
        current.account_id,
        AdminCapability::DisableAccount,
        AuditSubject::target("account", Some(id)),
        Some(serde_json::json!({ "newlyDisabled": newly })),
        now,
    )
    .await;
    // Target account id ONLY — never the target's email.
    tracing::info!(account_id = %current.account_id, target_account_id = %id, "admin disabled account");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/admin/accounts/{id}/reenable` → [`AdminCapability::ReenableAccount`].
/// The ONLY path that clears `disabled_at` (a self-sign-in never does).
async fn reenable_account(
    State(state): State<AppState>,
    AdminAccount(current): AdminAccount,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let now = state.clock.now_epoch_millis();
    let outcome = match state.accounts.reenable(id, now).await {
        Ok(outcome) => outcome,
        Err(sqlx::Error::RowNotFound) => return Err(ApiError::AccountNotFound),
        Err(err) => return Err(err.into()),
    };
    let newly = matches!(outcome, ReenableOutcome::Reenabled);
    audit(
        &state,
        current.account_id,
        AdminCapability::ReenableAccount,
        AuditSubject::target("account", Some(id)),
        Some(serde_json::json!({ "newlyReenabled": newly })),
        now,
    )
    .await;
    tracing::info!(account_id = %current.account_id, target_account_id = %id, "admin re-enabled account");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/admin/audit-log?limit=&cursor=` → [`AdminCapability::ViewAuditLog`]
/// One page of the consolidated security audit log,
/// newest-first, for review. Reads `audit_log` ONLY (never `session_events`,
/// never a per-user secret).
///
/// Paged so the append-only history — which only ever grows — does not have to
/// be serialized whole per view.
///
/// The read is ITSELF audited (one `view-audit-log` row per read), consistent
/// with `list_abuse_reports`'s `ViewReports` self-logging: an admin inspecting
/// the audit log is itself a security-relevant admin action.
async fn list_audit_log(
    State(state): State<AppState>,
    AdminAccount(current): AdminAccount,
    AppStrictQuery(params): AppStrictQuery<AuditQuery>,
) -> Result<Json<AuditLogPageBody>, ApiError> {
    // Every filter is parsed (and refused if malformed) BEFORE the read, so an
    // unusable filter can never silently widen the result to the whole log.
    let (actor, action, object) = (params.actor()?, params.action()?, params.object()?);
    let fingerprint = params.fingerprint()?;
    let page = state
        .audit_log
        .list_page_filtered(
            clamp_limit(params.limit),
            params.cursor()?,
            actor,
            action,
            object,
        )
        .await?;
    let now = state.clock.now_epoch_millis();
    audit(
        &state,
        current.account_id,
        AdminCapability::ViewAuditLog,
        AuditSubject::NONE,
        None,
        now,
    )
    .await;
    tracing::info!(account_id = %current.account_id, "admin viewed audit log");

    let items = page
        .rows
        .into_iter()
        .map(|e| AuditLogView {
            id: e.id,
            occurred_at: rfc3339(e.occurred_at_millis),
            actor_account_id: e.actor_account_id,
            action: e.action,
            target_type: e.target_type,
            target_id: e.target_id,
            metadata: e.metadata,
        })
        .collect();
    Ok(Json(AuditLogPageBody {
        items,
        // Stamped with the filters it was issued under, so replaying it against
        // a different set is refused rather than silently re-anchoring.
        next_cursor: page.next.map(|c| encode_cursor_for(c, fingerprint)),
    }))
}
