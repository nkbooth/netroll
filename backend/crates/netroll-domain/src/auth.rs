// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Pure authentication decisions: magic-link and session verdicts, the
//! method-linking invariant, email normalization, and token hashing.
//!
//! Everything here is clock-injected and I/O-free; effects (issuing tokens,
//! sending mail, storage) live in adapters and the app layer.

use uuid::Uuid;

use crate::model::account::Account;

/// How long an issued magic link stays consumable (pinned: 15 minutes).
pub const MAGIC_LINK_TTL_MILLIS: u64 = 15 * 60 * 1000;
/// Sliding idle window after which a session is rejected (pinned: 14 days).
pub const SESSION_IDLE_MILLIS: u64 = 14 * 24 * 60 * 60 * 1000;
/// Hard cap on total session lifetime (pinned: 30 days).
pub const SESSION_ABSOLUTE_MILLIS: u64 = 30 * 24 * 60 * 60 * 1000;

/// Normalizes an email address for identity comparison and storage:
/// surrounding whitespace stripped, ASCII letters lowercased.
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// SHA-256 of a raw token. Only this hash is ever stored; the raw token
/// exists solely in the emailed link or the session cookie.
pub fn hash_token(raw: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(raw).into()
}

/// Domain view of a stored magic-link token, times as epoch millis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicLinkToken {
    /// Normalized email the link was requested for.
    pub email: String,
    /// Instant the link stops being consumable.
    pub expires_at_millis: u64,
    /// Set once the link has been used; single-use.
    pub consumed_at_millis: Option<u64>,
}

/// Outcome of presenting a magic-link token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagicLinkVerdict {
    /// Unconsumed and unexpired — sign-in may proceed.
    Valid,
    /// Past its TTL.
    Expired,
    /// Already used once; second use is rejected.
    Consumed,
    /// No token matches the presented hash.
    Invalid,
}

/// Shared single-use-link lifecycle judgment for magic-link and
/// email-change confirmation tokens (both are single-use, TTL-bounded
/// links, so the precedence is identical — one source of truth).
///
/// Precedence: a consumed link stays `Consumed` even after it also expires
/// (the more truthful refusal); then expiry (the boundary instant is
/// `Expired`); otherwise `Valid`. A missing token is the caller's `Invalid`.
fn link_verdict(
    expires_at_millis: u64,
    consumed_at_millis: Option<u64>,
    now_millis: u64,
) -> MagicLinkVerdict {
    if consumed_at_millis.is_some() {
        MagicLinkVerdict::Consumed
    } else if now_millis >= expires_at_millis {
        MagicLinkVerdict::Expired
    } else {
        MagicLinkVerdict::Valid
    }
}

/// Judges a presented token against the injected `now`.
///
/// Precedence: a missing token is `Invalid`; a consumed token stays
/// `Consumed` even after it also expires (the more truthful refusal);
/// then expiry; otherwise `Valid`.
pub fn magic_link_verdict(token: Option<&MagicLinkToken>, now_millis: u64) -> MagicLinkVerdict {
    match token {
        None => MagicLinkVerdict::Invalid,
        Some(t) => link_verdict(t.expires_at_millis, t.consumed_at_millis, now_millis),
    }
}

/// Domain view of a stored email-change confirmation token, times as epoch
/// millis. Unlike [`MagicLinkToken`] (email-keyed, account may not exist
/// yet) this token retargets an existing `account_id` to `new_email` and is
/// meaningless without both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailChangeToken {
    /// The signed-in account that requested the change.
    pub account_id: Uuid,
    /// Normalized address the account moves TO once the link is consumed.
    pub new_email: String,
    /// Instant the link stops being consumable.
    pub expires_at_millis: u64,
    /// Set once the link has been used; single-use.
    pub consumed_at_millis: Option<u64>,
}

/// Judges a presented email-change confirmation link — an email-change
/// confirmation link IS a single-use magic link, so it reuses
/// [`MagicLinkVerdict`] and the shared [`link_verdict`] lifecycle rule
/// (identical precedence to [`magic_link_verdict`]).
pub fn email_change_verdict(token: Option<&EmailChangeToken>, now_millis: u64) -> MagicLinkVerdict {
    match token {
        None => MagicLinkVerdict::Invalid,
        Some(t) => link_verdict(t.expires_at_millis, t.consumed_at_millis, now_millis),
    }
}

/// Domain view of a stored session row, times as epoch millis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    /// Last request that touched this session (sliding idle cursor).
    pub last_seen_at_millis: u64,
    /// Hard lifetime cap, fixed at creation.
    pub absolute_expires_at_millis: u64,
    /// Set when revoked server-side (sign-out).
    pub revoked_at_millis: Option<u64>,
}

/// Outcome of presenting a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionVerdict {
    /// Live session — the request is authenticated.
    Valid,
    /// Revoked, idle-expired, or past its absolute cap.
    Rejected,
}

/// Judges a session against the injected `now`: revoked, past the absolute
/// cap (`now >= absolute_expires_at`), or idle for `idle_window_millis` or
/// longer is `Rejected`; otherwise `Valid`.
pub fn session_verdict(
    session: &SessionState,
    idle_window_millis: u64,
    now_millis: u64,
) -> SessionVerdict {
    let revoked = session.revoked_at_millis.is_some();
    let past_cap = now_millis >= session.absolute_expires_at_millis;
    let idle = now_millis.saturating_sub(session.last_seen_at_millis) >= idle_window_millis;
    if revoked || past_cap || idle {
        SessionVerdict::Rejected
    } else {
        SessionVerdict::Valid
    }
}

/// Proof that the bearer controls an email address: a magic link for exactly
/// this (normalized) email was consumed in the current flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailControlProof {
    /// The normalized email the consumed link was issued for.
    pub email: String,
}

/// The linking invariant: a magic-link
/// method may attach to an account only with proof of control of that
/// account's email — an email string match alone never attaches.
pub fn may_attach_magic_link(account: &Account, proof: Option<&EmailControlProof>) -> bool {
    matches!(proof, Some(p) if p.email == account.email)
}

/// What the app must persist after a magic link is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumptionOutcome {
    /// No account exists for the email: create it, mark the email verified,
    /// and attach the magic-link method.
    CreateAccountVerifyAndAttach {
        /// Normalized email for the new account.
        email: String,
    },
    /// An account already exists: mark its email verified and attach the
    /// magic-link method (idempotently).
    VerifyAndAttach {
        /// The existing account to attach to.
        account_id: Uuid,
    },
}

/// Decides the post-consumption action. Attachment to an existing account is
/// gated on [`may_attach_magic_link`]: only proven control of that account's
/// email attaches — a candidate account the proof does not cover is
/// treated as "no account" rather than attached to by string coincidence.
pub fn on_magic_link_consumed(
    proof: &EmailControlProof,
    existing: Option<&Account>,
) -> ConsumptionOutcome {
    match existing {
        Some(account) if may_attach_magic_link(account, Some(proof)) => {
            ConsumptionOutcome::VerifyAndAttach {
                account_id: account.id,
            }
        }
        _ => ConsumptionOutcome::CreateAccountVerifyAndAttach {
            email: proof.email.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(email: &str) -> Account {
        Account {
            id: Uuid::now_v7(),
            email: email.into(),
            email_verified_at_millis: None,
            callsign: None,
            display_name: None,
            location: None,
            grid: None,
            avatar_url: None,
            deleted_at_millis: None,
            disabled_at_millis: None,
        }
    }

    #[test]
    fn normalize_trims_and_ascii_lowercases() {
        assert_eq!(normalize_email("  Op@Example.COM \n"), "op@example.com");
        assert_eq!(
            normalize_email("already@lower.example.net"),
            "already@lower.example.net"
        );
    }

    #[test]
    fn hash_token_is_sha256() {
        // NIST test vector: SHA-256("abc").
        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(hash_token(b"abc"), expected);
    }

    #[test]
    fn unknown_token_is_invalid() {
        assert_eq!(magic_link_verdict(None, 1_000), MagicLinkVerdict::Invalid);
    }

    #[test]
    fn expired_token_is_expired_including_the_boundary_instant() {
        let token = MagicLinkToken {
            email: "op@example.com".into(),
            expires_at_millis: 1_000,
            consumed_at_millis: None,
        };
        assert_eq!(
            magic_link_verdict(Some(&token), 1_000),
            MagicLinkVerdict::Expired
        );
        assert_eq!(
            magic_link_verdict(Some(&token), 2_000),
            MagicLinkVerdict::Expired
        );
    }

    #[test]
    fn consumed_token_is_consumed_even_when_it_has_also_expired() {
        let token = MagicLinkToken {
            email: "op@example.com".into(),
            expires_at_millis: 1_000,
            consumed_at_millis: Some(500),
        };
        assert_eq!(
            magic_link_verdict(Some(&token), 900),
            MagicLinkVerdict::Consumed
        );
        assert_eq!(
            magic_link_verdict(Some(&token), 5_000),
            MagicLinkVerdict::Consumed
        );
    }

    #[test]
    fn live_unconsumed_token_is_valid() {
        let token = MagicLinkToken {
            email: "op@example.com".into(),
            expires_at_millis: 1_000,
            consumed_at_millis: None,
        };
        assert_eq!(
            magic_link_verdict(Some(&token), 999),
            MagicLinkVerdict::Valid
        );
    }

    fn email_change_token(
        expires_at_millis: u64,
        consumed_at_millis: Option<u64>,
    ) -> EmailChangeToken {
        EmailChangeToken {
            account_id: Uuid::now_v7(),
            new_email: "new@example.com".into(),
            expires_at_millis,
            consumed_at_millis,
        }
    }

    #[test]
    fn unknown_email_change_token_is_invalid() {
        assert_eq!(email_change_verdict(None, 1_000), MagicLinkVerdict::Invalid);
    }

    #[test]
    fn expired_email_change_token_is_expired_including_the_boundary_instant() {
        let token = email_change_token(1_000, None);
        assert_eq!(
            email_change_verdict(Some(&token), 1_000),
            MagicLinkVerdict::Expired
        );
        assert_eq!(
            email_change_verdict(Some(&token), 2_000),
            MagicLinkVerdict::Expired
        );
    }

    #[test]
    fn consumed_email_change_token_is_consumed_even_when_it_has_also_expired() {
        let token = email_change_token(1_000, Some(500));
        assert_eq!(
            email_change_verdict(Some(&token), 900),
            MagicLinkVerdict::Consumed
        );
        assert_eq!(
            email_change_verdict(Some(&token), 5_000),
            MagicLinkVerdict::Consumed
        );
    }

    #[test]
    fn live_unconsumed_email_change_token_is_valid() {
        let token = email_change_token(1_000, None);
        assert_eq!(
            email_change_verdict(Some(&token), 999),
            MagicLinkVerdict::Valid
        );
    }

    fn live_session(now: u64) -> SessionState {
        SessionState {
            last_seen_at_millis: now,
            absolute_expires_at_millis: now + SESSION_ABSOLUTE_MILLIS,
            revoked_at_millis: None,
        }
    }

    #[test]
    fn fresh_session_is_valid() {
        let session = live_session(10_000);
        assert_eq!(
            session_verdict(&session, SESSION_IDLE_MILLIS, 10_500),
            SessionVerdict::Valid
        );
    }

    #[test]
    fn revoked_session_is_rejected() {
        let session = SessionState {
            revoked_at_millis: Some(10_100),
            ..live_session(10_000)
        };
        assert_eq!(
            session_verdict(&session, SESSION_IDLE_MILLIS, 10_500),
            SessionVerdict::Rejected
        );
    }

    #[test]
    fn session_past_its_absolute_cap_is_rejected_including_the_boundary() {
        let session = SessionState {
            // Keep the idle window satisfied so only the cap can reject.
            last_seen_at_millis: 50_000,
            absolute_expires_at_millis: 50_000,
            revoked_at_millis: None,
        };
        assert_eq!(
            session_verdict(&session, SESSION_IDLE_MILLIS, 50_000),
            SessionVerdict::Rejected
        );
        assert_eq!(
            session_verdict(&session, SESSION_IDLE_MILLIS, 49_999),
            SessionVerdict::Valid
        );
    }

    #[test]
    fn idle_session_is_rejected_once_the_window_elapses() {
        let session = SessionState {
            last_seen_at_millis: 10_000,
            absolute_expires_at_millis: u64::MAX,
            revoked_at_millis: None,
        };
        let window = 5_000;
        assert_eq!(
            session_verdict(&session, window, 14_999),
            SessionVerdict::Valid
        );
        assert_eq!(
            session_verdict(&session, window, 15_000),
            SessionVerdict::Rejected
        );
    }

    #[test]
    fn email_string_match_alone_never_attaches_a_method() {
        let acct = account("op@example.com");
        assert!(!may_attach_magic_link(&acct, None));
    }

    #[test]
    fn proof_for_a_different_email_does_not_attach() {
        let acct = account("op@example.com");
        let proof = EmailControlProof {
            email: "other@example.com".into(),
        };
        assert!(!may_attach_magic_link(&acct, Some(&proof)));
    }

    #[test]
    fn consumed_link_proof_for_the_account_email_attaches() {
        let acct = account("op@example.com");
        let proof = EmailControlProof {
            email: "op@example.com".into(),
        };
        assert!(may_attach_magic_link(&acct, Some(&proof)));
    }

    fn proof(email: &str) -> EmailControlProof {
        EmailControlProof {
            email: email.into(),
        }
    }

    #[test]
    fn consuming_a_link_with_no_account_creates_verifies_and_attaches() {
        assert_eq!(
            on_magic_link_consumed(&proof("new@example.com"), None),
            ConsumptionOutcome::CreateAccountVerifyAndAttach {
                email: "new@example.com".into()
            }
        );
    }

    #[test]
    fn consuming_a_link_with_an_existing_account_verifies_and_attaches_to_it() {
        let acct = account("op@example.com");
        assert_eq!(
            on_magic_link_consumed(&proof("op@example.com"), Some(&acct)),
            ConsumptionOutcome::VerifyAndAttach {
                account_id: acct.id
            }
        );
    }

    #[test]
    fn consuming_a_link_never_attaches_to_an_account_with_a_different_email() {
        // The proof gates attachment: a candidate account whose email
        // does not match the proven email must not gain the method.
        let acct = account("other@example.com");
        assert_eq!(
            on_magic_link_consumed(&proof("op@example.com"), Some(&acct)),
            ConsumptionOutcome::CreateAccountVerifyAndAttach {
                email: "op@example.com".into()
            }
        );
    }
}
