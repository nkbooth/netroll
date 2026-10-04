// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Postgres repositories: compile-time-checked sqlx queries, no ORM.
//!
//! Times cross this boundary as epoch millis (the domain's currency) and are
//! stored as `timestamptz`.

pub mod abuse_reports;
pub mod accounts;
pub mod admin_search;
pub mod audit_log;
pub mod consents;
pub mod delivery_configs;
pub mod delivery_jobs;
pub mod discovery;
pub mod email_changes;
pub mod favorites;
pub mod health;
pub mod magic_links;
pub mod net_connections;
pub mod net_definitions;
pub mod net_session_roles;
pub mod net_sessions;
pub mod qrz_credentials;
pub mod roster_memory;
pub mod schedules;
pub mod session_events;
pub mod sessions;

use chrono::{DateTime, Utc};
use netroll_domain::admin::PageCursor;

/// A stored session record this version of NetRoll can no longer read.
///
/// Boxed inside `sqlx::Error::Decode`, which is what makes it recoverable: the
/// HTTP layer's `From<sqlx::Error> for ApiError` downcasts to this type, so the
/// refusal cannot be swallowed into the generic "storage failed" 500 that every
/// other decode fault becomes. That distinction is the whole reason this type
/// exists rather than another formatted string.
///
/// It marks exactly two record shapes, both written before the connection set: a
/// `net_sessions.definition_snapshot` with no `connections` key, and a
/// `session_events.payload` for `session.started`/`frequency.changed` written
/// when a session had ONE frequency rather than a set of connections. Neither
/// is translated — there is no connection in such a record to attribute a
/// frequency to, and inventing one would publish a way to reach a net that
/// nobody declared.
///
/// **Exactly those two, and the writers check for them by name.** Mapping every
/// decode failure here would be wrong in a way the operator cannot see past:
/// this error's user-facing copy says *"Nothing you do will bring it back"*, and
/// genuine corruption, a writer regression, or a row written by a NEWER deploy
/// that a rollback is now reading are all recoverable. Those answer as ordinary
/// storage failures instead.
///
/// `Display`/`Error` are hand-written rather than derived so this crate does not
/// grow a `thiserror` dependency for one six-line impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreplayableLog {
    /// The stored column that could not be read, for the server log only —
    /// never for the operator, who is told what happened, not which column.
    pub what: String,
}

impl std::fmt::Display for UnreplayableLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} predates the connection set and can no longer be replayed",
            self.what
        )
    }
}

impl std::error::Error for UnreplayableLog {}

/// Whether a storage error is the one PERMANENT refusal.
///
/// Reads the boxed marker straight off `sqlx::Error::Decode`. Lives beside the
/// marker rather than in each caller: the WebSocket handlers, the on-close
/// delivery task and anything else holding a raw `sqlx::Error` all have to ask
/// the same question, and three hand-rolled `matches!`/downcast pairs are three
/// chances for one of them to answer differently. Callers holding an `ApiError`
/// have its own variant and do not need this.
pub fn is_unreplayable_log(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Decode(inner) if inner.downcast_ref::<UnreplayableLog>().is_some()
    )
}

/// One page of a keyset-paginated read.
///
/// `next` is `Some` only when the underlying table actually held another row —
/// the repo probes for it by fetching `limit + 1` and discarding the extra — so
/// a caller can offer "load more" without a wasted round trip that returns
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The rows for this page, at most `limit` of them, in the read's own order.
    pub rows: Vec<T>,
    /// Where the next page resumes, or `None` when this page is the last.
    pub next: Option<PageCursor>,
}

/// Splits an over-fetched `limit + 1` result into a page plus its next cursor.
///
/// `key` reads the ordering `(timestamp, id)` off a row — the same pair the
/// query's `ORDER BY` uses, which is what makes the cursor total.
fn into_page<T>(mut rows: Vec<T>, limit: usize, key: impl Fn(&T) -> (u64, uuid::Uuid)) -> Page<T> {
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next = if has_more {
        rows.last().map(|row| {
            let (at_millis, id) = key(row);
            PageCursor { at_millis, id }
        })
    } else {
        None
    };
    Page { rows, next }
}

/// Escapes `term` so it reads as a LITERAL inside a `LIKE`/`ILIKE` pattern
/// whose statement declares `ESCAPE '\'`.
///
/// Without this a `%` or `_` a user typed would silently become a wildcard —
/// `_` matching any character turns a precise-looking lookup into a broader one
/// than the caller asked for — and a bare `\` would swallow the character after
/// it. Shared here rather than per call site: four sites across two files test
/// user text against a column this way, and one of them escaping differently
/// is a wildcard nobody can see in a green suite.
pub(crate) fn like_literal(term: &str) -> String {
    term.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// The `%…%` pattern that makes `col ILIKE $n ESCAPE '\'` a case-insensitive
/// SUBSTRING test for `term`, with `term`'s own `%`, `_` and `\` kept literal.
///
/// Built in the adapter, never in a handler or the domain: the wire echoes the
/// user's raw term, and the pattern is a storage detail of how it is matched.
pub(crate) fn like_contains(term: &str) -> String {
    format!("%{}%", like_literal(term))
}

fn utc_from_millis(millis: u64) -> DateTime<Utc> {
    // In-range for any timestamp this app can produce (fails past year 262143).
    // The one caller-controlled source of millis — a page cursor — is range
    // checked in `netroll_domain::admin::parse_cursor`, so a client cannot reach
    // this.
    DateTime::from_timestamp_millis(millis as i64).expect("epoch millis in chrono range")
}

fn millis_from_utc(instant: DateTime<Utc>) -> u64 {
    // Stored timestamps are never pre-1970; clamp instead of wrapping.
    instant.timestamp_millis().max(0) as u64
}
