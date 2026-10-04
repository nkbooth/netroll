// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Two pure bot checks with no external dependency: a honeypot field a real
//! user never fills, and a timing token whose HMAC-signed render instant
//! catches a submission too fast, too late or forged. A `Bot` verdict is
//! silently dropped, so nothing here is user-facing — naming the failing check
//! would teach bots to route around it.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Verdict of a form submission under the honeypot + timing checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotVerdict {
    /// Every applied check passed — process the submission normally.
    Human,
    /// A check failed — silently drop the submission (success-shaped response).
    Bot,
}

/// Default floor on plausible human form-fill time. A submission arriving sooner
/// than this after the form was rendered is scripted. Tuned low enough that even
/// a fast human pasting an email clears it, high enough that an instant
/// programmatic POST does not. Tunable, like the rate-limiter quotas.
pub const DEFAULT_MIN_FILL_MILLIS: u64 = 1_200;

/// Default form-token lifetime. Generous so a human who leaves the tab open a
/// while still submits within it; bounds token replay and clock drift. Tunable.
pub const DEFAULT_FORM_TOKEN_TTL_MILLIS: u64 = 2 * 60 * 60 * 1_000;

/// HMAC-SHA256 keyed by the raw bot-mitigation secret over the token payload.
type FormMac = Hmac<Sha256>;

/// Issues a signed, timestamped form token embedding `issued_at_millis`.
///
/// Shape is `"<issued_at_millis>.<lowercase-hex-hmac>"` — stateless (no DB): the
/// timestamp travels in the token and the HMAC (keyed by `secret`) makes it
/// unforgeable. The caller hands this to the client when the form renders and
/// verifies it back with [`verify_form_submission`] on submit.
pub fn issue_form_token(secret: &[u8], issued_at_millis: u64) -> String {
    let payload = issued_at_millis.to_string();
    let sig = sign(secret, payload.as_bytes());
    format!("{payload}.{sig}")
}

/// HMAC-SHA256 of `msg` under `secret`, as lowercase hex.
fn sign(secret: &[u8], msg: &[u8]) -> String {
    let mut mac = FormMac::new_from_slice(secret).expect("HMAC accepts a key of any length");
    mac.update(msg);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Verifies a form submission against the honeypot and timing checks.
///
/// Returns [`BotVerdict::Bot`] when: the honeypot field is non-empty; the token
/// is absent, malformed, or has a forged/tampered signature; the submission
/// arrived sooner than `min_fill_millis` after the token was issued; or the
/// token is older than `ttl_millis`. Otherwise [`BotVerdict::Human`]. `secret`
/// is the env-injected signing key; `now_millis` comes from the injected clock.
pub fn verify_form_submission(
    secret: &[u8],
    form_token: Option<&str>,
    honeypot: Option<&str>,
    now_millis: u64,
    min_fill_millis: u64,
    ttl_millis: u64,
) -> BotVerdict {
    // A real user never fills the hidden field; a naive bot autofill does. NOT
    // trimmed: a bot padding the field with whitespace to dodge an
    // `is_empty()`-after-`trim()` check must not evade detection — any
    // non-empty value, whitespace included, is a bot.
    if honeypot.is_some_and(|value| !value.is_empty()) {
        return BotVerdict::Bot;
    }
    // The token must be present, well-formed, and integrity-signed.
    let Some(token) = form_token else {
        return BotVerdict::Bot;
    };
    let Some((payload, sig)) = token.split_once('.') else {
        return BotVerdict::Bot;
    };
    let Ok(issued_at_millis) = payload.parse::<u64>() else {
        return BotVerdict::Bot;
    };
    if !verify_signature(secret, payload.as_bytes(), sig) {
        return BotVerdict::Bot;
    }
    // Timing floor + ceiling. `saturating_sub` makes a future-dated (skewed or
    // forged) timestamp read as elapsed 0, which the floor then rejects.
    let elapsed = now_millis.saturating_sub(issued_at_millis);
    if elapsed < min_fill_millis || elapsed > ttl_millis {
        return BotVerdict::Bot;
    }
    BotVerdict::Human
}

/// Constant-time verification of a lowercase-hex HMAC tag over `msg`.
fn verify_signature(secret: &[u8], msg: &[u8], provided_hex: &str) -> bool {
    let Some(provided) = decode_hex(provided_hex) else {
        return false;
    };
    let mut mac = FormMac::new_from_slice(secret).expect("HMAC accepts a key of any length");
    mac.update(msg);
    // `verify_slice` is constant-time — the tag covers an attacker-chosen
    // timestamp, so a timing oracle on the compare would be a forgery seam.
    mac.verify_slice(&provided).is_ok()
}

/// Decodes an even-length lowercase/uppercase hex string to bytes, or `None` if
/// it is not valid hex.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"instance-bot-mitigation-secret";
    const MIN_FILL: u64 = 1_200;
    const TTL: u64 = 2 * 60 * 60 * 1_000;

    #[test]
    fn a_well_timed_submission_with_an_empty_honeypot_is_human() {
        let issued = 1_000_000;
        let token = issue_form_token(SECRET, issued);
        let now = issued + MIN_FILL + 500;
        assert_eq!(
            verify_form_submission(SECRET, Some(&token), Some(""), now, MIN_FILL, TTL),
            BotVerdict::Human
        );
        // An absent honeypot field is equivalent to an empty one.
        assert_eq!(
            verify_form_submission(SECRET, Some(&token), None, now, MIN_FILL, TTL),
            BotVerdict::Human
        );
    }

    #[test]
    fn a_whitespace_only_honeypot_is_a_bot() {
        // A bot padding the field with a space to slip past a naive
        // `trim().is_empty()` check must still be caught.
        let issued = 1_000_000;
        let token = issue_form_token(SECRET, issued);
        let now = issued + MIN_FILL + 500;
        assert_eq!(
            verify_form_submission(SECRET, Some(&token), Some(" "), now, MIN_FILL, TTL),
            BotVerdict::Bot
        );
    }

    #[test]
    fn a_filled_honeypot_is_a_bot_even_with_a_valid_token_and_timing() {
        let issued = 1_000_000;
        let token = issue_form_token(SECRET, issued);
        let now = issued + MIN_FILL + 500;
        assert_eq!(
            verify_form_submission(
                SECRET,
                Some(&token),
                Some("http://spam.example"),
                now,
                MIN_FILL,
                TTL
            ),
            BotVerdict::Bot
        );
    }

    #[test]
    fn a_missing_token_is_a_bot() {
        assert_eq!(
            verify_form_submission(SECRET, None, Some(""), 9_999_999, MIN_FILL, TTL),
            BotVerdict::Bot
        );
    }

    #[test]
    fn a_malformed_token_is_a_bot() {
        for bad in ["no-dot", "notanumber.deadbeef", ".", "123."] {
            assert_eq!(
                verify_form_submission(SECRET, Some(bad), Some(""), 9_999_999, MIN_FILL, TTL),
                BotVerdict::Bot,
                "expected Bot for malformed token {bad:?}"
            );
        }
    }

    #[test]
    fn a_token_signed_with_a_different_secret_is_a_bot() {
        let issued = 1_000_000;
        let token = issue_form_token(b"attacker-guessed-secret", issued);
        let now = issued + MIN_FILL + 500;
        assert_eq!(
            verify_form_submission(SECRET, Some(&token), Some(""), now, MIN_FILL, TTL),
            BotVerdict::Bot
        );
    }

    #[test]
    fn tampering_the_timestamp_while_keeping_the_old_signature_is_a_bot() {
        let issued = 1_000_000;
        let token = issue_form_token(SECRET, issued);
        let (_, sig) = token.split_once('.').expect("well-formed");
        // Forge an earlier issued-at (to fake a longer fill time) but reuse the
        // signature bound to the original timestamp.
        let forged = format!("{}.{sig}", issued - MIN_FILL);
        let now = issued + 10; // would be "too fast" honestly, but the forge
        assert_eq!(
            verify_form_submission(SECRET, Some(&forged), Some(""), now, MIN_FILL, TTL),
            BotVerdict::Bot
        );
    }

    #[test]
    fn a_submission_faster_than_the_fill_floor_is_a_bot() {
        let issued = 1_000_000;
        let token = issue_form_token(SECRET, issued);
        let now = issued + MIN_FILL - 1;
        assert_eq!(
            verify_form_submission(SECRET, Some(&token), Some(""), now, MIN_FILL, TTL),
            BotVerdict::Bot
        );
    }

    #[test]
    fn a_submission_after_the_ttl_is_a_bot() {
        let issued = 1_000_000;
        let token = issue_form_token(SECRET, issued);
        let now = issued + TTL + 1;
        assert_eq!(
            verify_form_submission(SECRET, Some(&token), Some(""), now, MIN_FILL, TTL),
            BotVerdict::Bot
        );
    }

    #[test]
    fn the_exact_fill_floor_and_ttl_boundaries_are_human() {
        let issued = 1_000_000;
        let token = issue_form_token(SECRET, issued);
        assert_eq!(
            verify_form_submission(
                SECRET,
                Some(&token),
                Some(""),
                issued + MIN_FILL,
                MIN_FILL,
                TTL
            ),
            BotVerdict::Human,
            "elapsed == min_fill is admissible"
        );
        assert_eq!(
            verify_form_submission(SECRET, Some(&token), Some(""), issued + TTL, MIN_FILL, TTL),
            BotVerdict::Human,
            "elapsed == ttl is admissible"
        );
    }
}
