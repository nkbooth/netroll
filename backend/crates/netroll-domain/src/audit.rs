// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The CLOSED, finite vocabulary of non-admin security-audit actions. Every
//! non-admin source writes to the same `audit_log` as the admin actions, and
//! this enum is the closed set of their `action` verbs, one per event type, so
//! the review surface and the PII-free test reason over a finite set rather
//! than ad-hoc literals. That property is caller discipline, not a guarantee.

/// The CLOSED, finite set of non-admin security-audit actions.
///
/// One variant per consolidated event source: successful sign-in / session mint,
/// self sign-out, per-net role grant and revoke, and the account
/// self-deletion REQUEST — the actor-present, user-initiated event, never the
/// mechanical timer-driven hard-delete. The four platform-admin
/// actions keep their own closed vocabulary on
/// [`crate::admin::AdminCapability`]; together the two enums enumerate every
/// `action` the `audit_log` may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    /// A successful authentication — a session was minted for the actor.
    SignedIn,
    /// A self-initiated sign-out (session revocation).
    SignedOut,
    /// The account holder requested deletion of their OWN account (the
    /// actor-present self-delete request; the later hard-delete finalize is a
    /// mechanical consequence and is NOT separately audited).
    AccountSelfDeleted,
    /// A per-net role was granted to an account.
    RoleGranted,
    /// A per-net role was revoked from an account.
    RoleRevoked,
    /// A net definition was created.
    NetCreated,
    /// A net definition's fields were changed. Recorded only when something
    /// ACTUALLY changed — a no-op save is not an event.
    NetUpdated,
    /// A net definition was archived (the owner-facing "delete").
    NetArchived,
    /// A net session was started by an operator.
    SessionStarted,
    /// A net session was closed BY AN OPERATOR. The presence-monitor's
    /// abandoned-session auto-close is actorless and is deliberately NOT
    /// recorded here — see the module doc.
    SessionClosed,
}

impl AuditAction {
    /// Every non-admin audit action — the finite closed set. The cross-source
    /// PII-free test enumerates this (∪ [`crate::admin::AdminCapability::EVERY`])
    /// to prove every produced `action` is a known, bounded verb.
    pub const EVERY: [AuditAction; 10] = [
        AuditAction::SignedIn,
        AuditAction::SignedOut,
        AuditAction::AccountSelfDeleted,
        AuditAction::RoleGranted,
        AuditAction::RoleRevoked,
        AuditAction::NetCreated,
        AuditAction::NetUpdated,
        AuditAction::NetArchived,
        AuditAction::SessionStarted,
        AuditAction::SessionClosed,
    ];

    /// The stable lowercase-kebab wire/audit spelling written as the `action`
    /// column on an `audit_log` entry. These values are PERMANENT — once
    /// written they are the historical record.
    pub fn as_str(self) -> &'static str {
        match self {
            AuditAction::SignedIn => "signed-in",
            AuditAction::SignedOut => "signed-out",
            AuditAction::AccountSelfDeleted => "account-self-deleted",
            AuditAction::RoleGranted => "role-granted",
            AuditAction::RoleRevoked => "role-revoked",
            AuditAction::NetCreated => "net-created",
            AuditAction::NetUpdated => "net-updated",
            AuditAction::NetArchived => "net-archived",
            AuditAction::SessionStarted => "session-started",
            AuditAction::SessionClosed => "session-closed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_action_set_is_exactly_the_ten_consolidated_sources() {
        // The crux: the non-admin audit vocabulary is finite and closed.
        assert_eq!(AuditAction::EVERY.len(), 10);
        assert!(AuditAction::EVERY.contains(&AuditAction::SignedIn));
        assert!(AuditAction::EVERY.contains(&AuditAction::SignedOut));
        assert!(AuditAction::EVERY.contains(&AuditAction::AccountSelfDeleted));
        assert!(AuditAction::EVERY.contains(&AuditAction::RoleGranted));
        assert!(AuditAction::EVERY.contains(&AuditAction::RoleRevoked));
    }

    #[test]
    fn every_audit_action_has_a_distinct_stable_wire_spelling() {
        // Each action serializes to its own kebab `action` string; no two
        // collide (they are the actions this audit log records).
        let spellings: std::collections::HashSet<_> =
            AuditAction::EVERY.iter().map(|a| a.as_str()).collect();
        assert_eq!(
            spellings.len(),
            AuditAction::EVERY.len(),
            "audit action strings are distinct"
        );
    }

    #[test]
    fn the_wire_spellings_are_the_permanent_kebab_verbs() {
        // Pin the exact permanent spellings — these become historical audit
        // rows and must never drift.
        assert_eq!(AuditAction::SignedIn.as_str(), "signed-in");
        assert_eq!(AuditAction::SignedOut.as_str(), "signed-out");
        assert_eq!(
            AuditAction::AccountSelfDeleted.as_str(),
            "account-self-deleted"
        );
        assert_eq!(AuditAction::RoleGranted.as_str(), "role-granted");
        assert_eq!(AuditAction::RoleRevoked.as_str(), "role-revoked");
    }

    #[test]
    fn no_audit_action_spelling_collides_with_an_admin_capability_spelling() {
        // The two closed vocabularies partition the `audit_log.action` space;
        // an audit verb must never alias an admin capability verb.
        use crate::admin::AdminCapability;
        for action in AuditAction::EVERY {
            for cap in AdminCapability::EVERY {
                assert_ne!(
                    action.as_str(),
                    cap.as_str(),
                    "audit action and admin capability spellings must not collide"
                );
            }
        }
    }
}
