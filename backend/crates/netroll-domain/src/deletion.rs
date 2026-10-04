// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Pure account self-deletion policy: the grace window and the verdict that
//! decides whether a soft-deleted account is still recoverable, past its
//! window, or was never pending. Clock-injected and I/O-free — the effects,
//! the soft-delete mark and the hard-delete sweep, live in the adapter and app
//! layers.

/// How long a soft-deleted account stays recoverable by signing back in
/// (pinned: 15 minutes — the Architecture default).
///
/// Numerically equal to [`crate::auth::MAGIC_LINK_TTL_MILLIS`] today, but a
/// deliberately independent policy: one is the undelete window, the other the
/// link TTL. They are NOT aliased so a future change to either cannot
/// silently move the other.
pub const DELETION_GRACE_MILLIS: u64 = 15 * 60 * 1000;

/// Outcome of judging an account's `deleted_at` against the injected `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionVerdict {
    /// Not pending deletion (`deleted_at` is unset) — a live account.
    Active,
    /// Pending deletion and still within the grace window — recoverable by
    /// signing back in (which clears `deleted_at`).
    InGraceWindow,
    /// Pending deletion and past the grace window — must be hard-deleted;
    /// signing in resurrects nothing, it mints a fresh account.
    Finalizable,
}

/// Judges an account's pending-deletion state against the injected `now`.
///
/// Precedence mirrors [`crate::auth`]'s boundary convention: an unset
/// `deleted_at` is [`DeletionVerdict::Active`]; otherwise the account is
/// [`DeletionVerdict::InGraceWindow`] until `deleted_at + grace`, and the
/// boundary instant `deleted_at + grace` (and beyond) is
/// [`DeletionVerdict::Finalizable`] — the same "boundary instant is expired"
/// rule as the magic-link expiry verdict.
pub fn deletion_verdict(
    deleted_at_millis: Option<u64>,
    now_millis: u64,
    grace_millis: u64,
) -> DeletionVerdict {
    match deleted_at_millis {
        None => DeletionVerdict::Active,
        Some(deleted_at) if now_millis < deleted_at.saturating_add(grace_millis) => {
            DeletionVerdict::InGraceWindow
        }
        Some(_) => DeletionVerdict::Finalizable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_with_no_deleted_at_is_active() {
        assert_eq!(
            deletion_verdict(None, 5_000, DELETION_GRACE_MILLIS),
            DeletionVerdict::Active
        );
    }

    #[test]
    fn a_pending_account_inside_the_window_is_in_grace() {
        // deleted_at = 1_000, grace = 4_000 → window ends at 5_000.
        assert_eq!(
            deletion_verdict(Some(1_000), 3_000, 4_000),
            DeletionVerdict::InGraceWindow
        );
    }

    #[test]
    fn a_pending_account_past_the_window_is_finalizable() {
        // deleted_at = 1_000, grace = 4_000 → window ends at 5_000.
        assert_eq!(
            deletion_verdict(Some(1_000), 9_000, 4_000),
            DeletionVerdict::Finalizable
        );
    }

    #[test]
    fn the_last_instant_inside_the_window_is_still_in_grace() {
        // now == deleted_at + grace - 1 → still recoverable.
        assert_eq!(
            deletion_verdict(Some(1_000), 4_999, 4_000),
            DeletionVerdict::InGraceWindow
        );
    }

    #[test]
    fn the_boundary_instant_is_finalizable() {
        // now == deleted_at + grace → finalizable (boundary is expired).
        assert_eq!(
            deletion_verdict(Some(1_000), 5_000, 4_000),
            DeletionVerdict::Finalizable
        );
    }

    #[test]
    fn the_grace_constant_is_fifteen_minutes() {
        assert_eq!(DELETION_GRACE_MILLIS, 15 * 60 * 1000);
    }
}
