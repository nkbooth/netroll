// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! A signed-in participant's own recent check-ins, shaped for a widget.
//!
//! It reads `checkin.added` only, so a later edit or removal is not reflected,
//! and it excludes check-ins logged FOR you: matching a bare callsign against
//! `accounts.callsign` would hand a previous holder's history to today's.

use axum::extract::State;
use axum::{Extension, Json};
use netroll_domain::admin::{clamp_limit, encode_cursor};
use serde::Serialize;
use uuid::Uuid;

use crate::middleware::session::CurrentAccount;

use super::admin::PageQuery;
use super::problem::ApiError;
use super::{AppState, AppStrictQuery, rfc3339};

/// One page of the caller's own check-in history (camelCase wire).
///
/// `nextCursor` is explicitly `null` on the last page rather than omitted — the
/// one deliberate exception to the omit-optional-fields rule, because clients
/// branch on it to decide whether to offer "load more". Same envelope the admin
/// paged reads publish.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckInHistoryPageBody {
    items: Vec<CheckInHistoryItem>,
    next_cursor: Option<String>,
}

/// One history row.
///
/// Exactly six fields, and the narrowness is the point. Every field is stable
/// AT ADD TIME (see the module doc), and nothing here is withheld from the
/// public roster for this same row: `callsign` and the check-in instant are
/// already public on the live session view, and `netTitle`/`band`/`mode` are
/// net-level facts the caller attended.
///
/// `band`/`mode` are NULLABLE, because an internet-only net has neither. They
/// must never be read from a top-level snapshot key asserted NOT NULL to sqlx,
/// which fails at RUNTIME once the field lives in the connection set. The three
/// additions that would break parity with the public roster — the entering
/// operator, the net DEFINITION id, and the definition's link token — are absent
/// on purpose and are asserted absent.
///
/// `band`/`mode` used to read the net's first connection's; they now name the
/// connection THIS check-in came in on, which is the difference between
/// describing the net and describing
/// the check-in. `via` beside them names that way in, and is what a row whose
/// way in carries no band (EchoLink, a talkgroup, a reflector) actually has to
/// say. It is a LABEL: a snapshot-local UUID is not a fact this reader can use,
/// and no surface renders one.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckInHistoryItem {
    net_session_id: Uuid,
    net_title: String,
    band: Option<String>,
    mode: Option<String>,
    /// The label of the way in this check-in arrived on, or `null` — as that
    /// way in stood WHEN this check-in was logged: for
    /// an RF way in the frequency in the label is the one in force at this
    /// row's own `seq`, not the net's planned one and not the one it later
    /// moved to.
    via: Option<String>,
    callsign: String,
    checked_in_at: String,
}

/// `GET /api/accounts/me/check-ins?limit=&cursor=` — the caller's own self
/// check-ins, newest first, keyset-paginated.
///
/// Ownership is taken from `CurrentAccount` (the session cookie) and bound
/// directly into the read's `actor` predicate; the cursor can only NARROW the
/// result, never widen it, because it carries just an ordering position.
///
/// No new problem type is introduced. The failure modes are, exactly:
///
/// - **401** `/errors/unauthenticated` from `require_session`, which runs as a
///   `route_layer` and therefore rejects before any query parsing.
/// - **400** `application/problem+json` `/errors/validation` from
///   `PageQuery::cursor()` for a cursor this server did not issue.
/// - **400** `application/problem+json` `/errors/validation` from the
///   `AppStrictQuery` extractor for a malformed `limit` (`?limit=abc`) or a
///   structurally invalid query string. This used to be axum's own `text/plain`
///   rejection; every query-reading route in the crate is now on
///   the one contract — the ten in the `http` tree, plus the two `ws` upgrade
///   handlers — so all three
///   `PageQuery`/`AuditQuery` reads still answer identically; they just answer
///   in problem+json now.
/// - **400** `application/problem+json` `/errors/validation` from the same
///   extractor for an **unrecognised** parameter (`?limitt=5`), because
///   `PageQuery` is `#[serde(deny_unknown_fields)]` and this read is on the
///   *filtered* list. Its `detail` is
///   user-facing: `ProfilePage.tsx:1200-1201` renders it verbatim.
pub(super) async fn list_check_in_history(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentAccount>,
    AppStrictQuery(params): AppStrictQuery<PageQuery>,
) -> Result<Json<CheckInHistoryPageBody>, ApiError> {
    let page = state
        .session_events
        .self_check_ins_page(
            current.account_id,
            clamp_limit(params.limit),
            params.cursor()?,
        )
        .await?;

    let items = page
        .rows
        .into_iter()
        .map(|row| CheckInHistoryItem {
            net_session_id: row.session_id,
            net_title: row.net_title,
            band: row.band,
            mode: row.mode,
            via: row.via,
            callsign: row.callsign.as_str().to_owned(),
            checked_in_at: rfc3339(row.checked_in_at_millis),
        })
        .collect();
    Ok(Json(CheckInHistoryPageBody {
        items,
        next_cursor: page.next.map(encode_cursor),
    }))
}
