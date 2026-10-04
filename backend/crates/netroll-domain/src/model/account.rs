// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Identity model: email-identified accounts with zero-or-more attached
//! authentication methods.

use uuid::Uuid;

/// A registered operator account, identified by normalized email.
///
/// No username, no password: credentials are separate [`AuthMethod`] rows so
/// new methods attach without touching identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Normalized (trimmed, ASCII-lowercased) email address.
    pub email: String,
    /// When the email was proven controlled, as epoch millis; `None` never
    /// happens for a persisted account in practice — accounts are created at
    /// link-consume time — but the model allows it for future methods.
    pub email_verified_at_millis: Option<u64>,
    /// The operator's reserved base callsign, normalized by
    /// [`crate::callsign::parse_callsign`]. `None` is a first-class state —
    /// browse/favorite/view-live never require one.
    pub callsign: Option<String>,
    /// Chosen display name, validated by
    /// [`crate::profile::parse_display_name`].
    pub display_name: Option<String>,
    /// Free-text location ("Hartford, CT"), validated by
    /// [`crate::profile::parse_location`].
    pub location: Option<String>,
    /// Maidenhead grid locator in canonical form (`FN31pr47`), validated by
    /// [`crate::profile::parse_grid`].
    pub grid: Option<String>,
    /// User-supplied HTTPS avatar URL, validated by
    /// [`crate::profile::parse_avatar_url`]. When `None`, the effective
    /// avatar falls back to the Gravatar derived from [`Account::email`].
    pub avatar_url: Option<String>,
    /// Set when the account entered the pending-deletion window; `None` for a
    /// live account. Consumed by
    /// [`crate::deletion::deletion_verdict`].
    pub deleted_at_millis: Option<u64>,
    /// Set when an admin disabled the account; `None` for a
    /// live account. ORTHOGONAL to [`Account::deleted_at_millis`]: disabling is
    /// an admin enforcement action cleared ONLY by an admin re-enable, never by
    /// a self-sign-in (an abuser must not be able to un-disable themselves), and
    /// it never hard-deletes data on a timer.
    pub disabled_at_millis: Option<u64>,
}

/// The validated profile write shape for `PUT /api/accounts/me/profile`:
/// every field already parsed by `crate::profile`'s parse
/// functions — the storage adapter trusts its caller for format, the same
/// contract as `set_callsign`. `None` means "clear" (PUT-replace
/// semantics).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileFields {
    /// Validated display name, or `None` to clear.
    pub display_name: Option<String>,
    /// Validated location, or `None` to clear.
    pub location: Option<String>,
    /// Canonical grid locator, or `None` to clear.
    pub grid: Option<String>,
    /// Validated HTTPS avatar URL, or `None` to clear.
    pub avatar_url: Option<String>,
}

/// The vocabulary of authentication methods.
///
/// Stored as lowercase-kebab text (`'magic-link'`); OAuth/password variants
/// arrive as fast-follows without an identity migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethodKind {
    /// Single-use email magic link.
    MagicLink,
}

impl AuthMethodKind {
    /// The stable wire/storage spelling of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMethodKind::MagicLink => "magic-link",
        }
    }
}

impl TryFrom<&str> for AuthMethodKind {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "magic-link" => Ok(AuthMethodKind::MagicLink),
            _ => Err(()),
        }
    }
}

/// An authentication method attached to an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethod {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Owning account.
    pub account_id: Uuid,
    /// Which credential this row represents.
    pub kind: AuthMethodKind,
    /// Last successful use, as epoch millis.
    pub last_used_at_millis: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_method_kind_round_trips_through_its_storage_spelling() {
        let spelled = AuthMethodKind::MagicLink.as_str();
        assert_eq!(spelled, "magic-link");
        assert_eq!(
            AuthMethodKind::try_from(spelled),
            Ok(AuthMethodKind::MagicLink)
        );
    }

    #[test]
    fn unknown_kind_spellings_are_rejected() {
        assert!(AuthMethodKind::try_from("password").is_err());
        assert!(AuthMethodKind::try_from("MAGIC-LINK").is_err());
    }
}
