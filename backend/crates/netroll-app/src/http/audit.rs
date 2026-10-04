// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The one shared post-commit audit-append seam.
//!
//! Callers invoke this AFTER their effect has committed, so a failure is
//! swallowed and logged rather than propagated. The tracing line is static,
//! because sqlx error text can embed the row values it failed to insert.

use uuid::Uuid;

use netroll_adapters::pg::audit_log::AuditEntry;

use super::AppState;

/// WHAT an audited action acted on, and WHERE it happened.
///
/// The two are genuinely different questions and the admin object-filter needs
/// both: a role grant's TARGET is the grantee account, but it HAPPENED in a
/// session belonging to a net. Filtering by that net must reach it, which a
/// target alone can never do.
///
/// One struct rather than four positional parameters, because they are four
/// adjacent `Option`s of two types — silently swappable at eleven call-sites,
/// and a swap would misfile a row rather than fail to compile.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AuditSubject {
    /// The kind of thing acted on (`account`, `net-definition`, …).
    pub target_type: Option<&'static str>,
    /// The id of the thing acted on.
    pub target_id: Option<Uuid>,
    /// The net session the action happened in, when it happened in one.
    pub session: Option<Uuid>,
    /// The net definition the action concerns, when it concerns one.
    pub definition: Option<Uuid>,
}

impl AuditSubject {
    /// An action with no specific target — the admin read surfaces.
    pub const NONE: Self = Self {
        target_type: None,
        target_id: None,
        session: None,
        definition: None,
    };

    /// An action on a target that is not a net or a session (an account, a
    /// report, or a session-less auth event).
    pub fn target(target_type: &'static str, target_id: Option<Uuid>) -> Self {
        Self {
            target_type: Some(target_type),
            target_id,
            ..Self::NONE
        }
    }

    /// An action on a net definition — target and net context are the same id.
    pub fn definition(id: Uuid) -> Self {
        Self {
            target_type: Some("net-definition"),
            target_id: Some(id),
            session: None,
            definition: Some(id),
        }
    }

    /// An action on a net session, carrying the net it belongs to so a
    /// net-filtered read reaches it without knowing the session id.
    pub fn session(session_id: Uuid, definition_id: Uuid) -> Self {
        Self {
            target_type: Some("net-session"),
            target_id: Some(session_id),
            session: Some(session_id),
            definition: Some(definition_id),
        }
    }

    /// An action on an ACCOUNT that happened inside a session — the role
    /// grant/revoke shape, where target and context deliberately differ.
    pub fn account_in_session(account_id: Uuid, session_id: Uuid, definition_id: Uuid) -> Self {
        Self {
            target_type: Some("account"),
            target_id: Some(account_id),
            session: Some(session_id),
            definition: Some(definition_id),
        }
    }
}

/// Appends one security-audit record post-commit (actor / action / target /
/// timestamp / metadata / context), swallowing-and-logging an append failure
/// rather than surfacing it to the caller.
///
/// `action` is a stable lowercase-kebab verb from a domain-owned CLOSED
/// vocabulary — [`netroll_domain::admin::AdminCapability::as_str`] for admin
/// actions or [`netroll_domain::audit::AuditAction::as_str`] for the
/// consolidated auth/role/deletion sources — never a scattered ad-hoc literal.
/// `metadata`, when present, MUST be bounded non-secret literals (booleans, role
/// kebab verbs, uuids, and field NAMES — never a field VALUE, which would put
/// user-authored text into the log); it never carries email / token / QRZ / grid
/// The injected `now` is the audit timestamp — never the DB clock.
pub(crate) async fn append_audit(
    state: &AppState,
    actor: Uuid,
    action: &str,
    subject: AuditSubject,
    metadata: Option<serde_json::Value>,
    now: u64,
) {
    if let Err(_err) = state
        .audit_log
        .append(
            &AuditEntry {
                actor_account_id: actor,
                action: action.to_owned(),
                target_type: subject.target_type.map(|s| s.to_owned()),
                target_id: subject.target_id,
                metadata,
                context_session_id: subject.session,
                context_definition_id: subject.definition,
            },
            now,
        )
        .await
    {
        // Static line only — sqlx error text can embed row values.
        tracing::error!(
            action = action,
            "audit-log append failed after the effect already committed"
        );
    }
}
