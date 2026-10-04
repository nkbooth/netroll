// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Instance-configurable bot mitigation for signup and net creation.
//!
//! A thin wrapper over the pure [`netroll_domain::bot_mitigation`] honeypot and
//! timing checks. The env-injected HMAC secret is the on/off switch: with no
//! secret, [`BotMitigation::verify`] always answers [`BotVerdict::Human`].

use netroll_domain::bot_mitigation::{
    BotVerdict, DEFAULT_FORM_TOKEN_TTL_MILLIS, DEFAULT_MIN_FILL_MILLIS, issue_form_token,
    verify_form_submission,
};
use zeroize::Zeroizing;

/// Instance bot-mitigation control. Enabled only when an HMAC secret is present
/// (the `BOT_MITIGATION_SECRET` env var); otherwise a no-op that admits
/// everything, so signup + net creation behave exactly as they did before.
pub struct BotMitigation {
    /// Present ⇒ enabled. The HMAC signing key for form tokens; scrubbed on drop.
    secret: Option<Zeroizing<Vec<u8>>>,
    /// Floor on plausible human form-fill time.
    min_fill_millis: u64,
    /// Form-token lifetime.
    ttl_millis: u64,
}

impl BotMitigation {
    /// Disabled control: no secret, so every submission is admitted and no form
    /// token is issued. The default installed by `AppState::new`.
    pub fn disabled() -> Self {
        Self {
            secret: None,
            min_fill_millis: DEFAULT_MIN_FILL_MILLIS,
            ttl_millis: DEFAULT_FORM_TOKEN_TTL_MILLIS,
        }
    }

    /// Enabled control keyed by `secret` (from `BOT_MITIGATION_SECRET`), with the
    /// default timing thresholds. `main` builds this from the resolved config;
    /// integration tests inject a known secret.
    pub fn with_secret(secret: Zeroizing<Vec<u8>>) -> Self {
        Self {
            secret: Some(secret),
            min_fill_millis: DEFAULT_MIN_FILL_MILLIS,
            ttl_millis: DEFAULT_FORM_TOKEN_TTL_MILLIS,
        }
    }

    /// Whether mitigation is active on this instance.
    pub fn is_enabled(&self) -> bool {
        self.secret.is_some()
    }

    /// Issues a signed, timestamped form token for the client to attach on
    /// submit, or `None` when mitigation is disabled (the client then sends no
    /// token and verification is skipped).
    pub fn issue_token(&self, now_millis: u64) -> Option<String> {
        self.secret
            .as_ref()
            .map(|secret| issue_form_token(secret, now_millis))
    }

    /// Judges a submission. Returns [`BotVerdict::Human`] unconditionally when
    /// disabled; otherwise applies the honeypot + timing checks. The caller
    /// silently drops a [`BotVerdict::Bot`] (success-shaped response), never
    /// surfacing which check failed.
    pub fn verify(
        &self,
        form_token: Option<&str>,
        honeypot: Option<&str>,
        now_millis: u64,
    ) -> BotVerdict {
        match &self.secret {
            None => BotVerdict::Human,
            Some(secret) => verify_form_submission(
                secret,
                form_token,
                honeypot,
                now_millis,
                self.min_fill_millis,
                self.ttl_millis,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_control_admits_everything_and_issues_no_token() {
        let mitigation = BotMitigation::disabled();
        assert!(!mitigation.is_enabled());
        assert!(mitigation.issue_token(1_000_000).is_none());
        // Even a filled honeypot and a missing token pass when disabled — the
        // endpoint must behave exactly as before.
        assert_eq!(
            mitigation.verify(None, Some("spam"), 1_000_000),
            BotVerdict::Human
        );
    }

    #[test]
    fn an_enabled_control_issues_a_token_that_round_trips_to_human() {
        let mitigation = BotMitigation::with_secret(Zeroizing::new(b"secret".to_vec()));
        assert!(mitigation.is_enabled());
        let issued_at = 1_000_000;
        let token = mitigation
            .issue_token(issued_at)
            .expect("enabled issues a token");
        let now = issued_at + DEFAULT_MIN_FILL_MILLIS + 1;
        assert_eq!(
            mitigation.verify(Some(&token), Some(""), now),
            BotVerdict::Human
        );
    }

    #[test]
    fn an_enabled_control_rejects_a_filled_honeypot() {
        let mitigation = BotMitigation::with_secret(Zeroizing::new(b"secret".to_vec()));
        let issued_at = 1_000_000;
        let token = mitigation.issue_token(issued_at).expect("token");
        let now = issued_at + DEFAULT_MIN_FILL_MILLIS + 1;
        assert_eq!(
            mitigation.verify(Some(&token), Some("filled"), now),
            BotVerdict::Bot
        );
    }
}
