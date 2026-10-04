// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Per-net delivery-config validation: raw inbound values in, a
//! [`DeliveryConfigFields`] the adapter trusts out, and a field-level error
//! whose `Display` becomes the problem+json `detail`. Pure — the webhook HMAC
//! secret is minted in the app layer — and it reuses
//! [`crate::egress::validate_egress_url`] and [`crate::auth::normalize_email`].

use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

use crate::auth::normalize_email;
use crate::egress::{EgressPolicyError, validate_egress_url};
use crate::profile::{ProfileError, parse_bounded_text};

/// Maximum number of delivery email addresses a single net may target. A small
/// bounded set — the whole point of the fan-out is a handful of recipients, not
/// a mailing list — kept in the domain so both the count guard and any UI hint
/// read one source.
pub const MAX_DELIVERY_EMAILS: usize = 10;

/// Per-address length bound (RFC 5321 forward-path limit); anything longer
/// cannot be a real mailbox. Mirrors the app layer's `MAX_EMAIL_LEN`.
const MAX_EMAIL_CHARS: usize = 254;

/// The unvalidated inbound delivery config: the raw email list, the raw
/// optional generic webhook URL, and the raw optional Discord webhook URL,
/// exactly as they arrive on the wire.
#[derive(Debug, Clone, Default)]
pub struct RawDeliveryConfig {
    /// Raw delivery email entries (may contain blanks / mixed case).
    pub emails: Vec<String>,
    /// Raw webhook URL, or `None`/blank to clear it.
    pub webhook_url: Option<String>,
    /// Raw Discord channel-webhook URL, or `None`/blank to clear it.
    pub discord_webhook_url: Option<String>,
}

/// The validated delivery config the adapter trusts (the "adapter trusts its
/// caller" contract): normalized, deduplicated-of-blanks, bounded emails and a
/// syntactically-validated webhook URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryConfigFields {
    /// Normalized delivery addresses (may be empty — "no email targets").
    pub emails: Vec<String>,
    /// The validated webhook URL, or `None` when no webhook is configured.
    pub webhook_url: Option<String>,
    /// The validated Discord channel-webhook URL, or `None` when no Discord
    /// destination is configured.
    ///
    /// This value IS a credential: a Discord webhook carries its bearer token
    /// in the URL PATH, so anyone holding the string can post to the channel.
    /// It must never reach a log line, an error `Display`, or a problem+json
    /// `detail` — which is why the four `Discord*` variants of
    /// [`DeliveryConfigError`] echo a fixed reason and never the URL.
    pub discord_webhook_url: Option<String>,
}

impl DeliveryConfigFields {
    /// Whether this config arms NO delivery at all — no email targets, no
    /// generic webhook, and no Discord destination (the definition of
    /// "delivery off").
    /// Delivery skips a net in this state.
    ///
    /// A near-identical predicate lives in `netroll-app`'s deliverer, written
    /// against the repo read model rather than this validated write shape. Both
    /// must learn about a new destination kind; the deliverer's copy is now
    /// derived from an exhaustive `match` over its destination-kind enum so a
    /// fourth kind cannot be forgotten there silently.
    pub fn is_delivery_off(&self) -> bool {
        self.emails.is_empty() && self.webhook_url.is_none() && self.discord_webhook_url.is_none()
    }
}

/// Why a submitted delivery config was rejected — one variant per field and
/// reason. `Display` is `"<field>: <reason>"` so the HTTP `detail` names the
/// offending field. The webhook variants are DISTINCT per
/// [`EgressPolicyError`] reason. No variant echoes the submitted URL or email
/// back — the fixed reason text only (the `validate_egress_url` posture).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeliveryConfigError {
    /// More than [`MAX_DELIVERY_EMAILS`] addresses were supplied.
    #[error("delivery emails: too many addresses")]
    TooManyEmails,
    /// An address failed the bounded-text guard (too long / control char).
    #[error("delivery email: {0}")]
    Email(ProfileError),
    /// An address failed the minimal structural check (not exactly one `@`,
    /// empty local/domain, or embedded whitespace).
    #[error("delivery email: is not a valid address")]
    EmailMalformed,
    /// The webhook URL scheme was not `https` ([`EgressPolicyError::NotHttps`]).
    #[error("webhook url: must use https")]
    WebhookNotHttps,
    /// The webhook URL had no host ([`EgressPolicyError::MissingHost`]).
    #[error("webhook url: has no host")]
    WebhookMissingHost,
    /// The webhook URL is a bare IP-literal in a denied range
    /// ([`EgressPolicyError::BlockedAddress`]).
    #[error("webhook url: resolves to a blocked address")]
    WebhookBlockedAddress,
    /// The webhook URL did not parse ([`EgressPolicyError::MalformedUrl`]).
    #[error("webhook url: is malformed")]
    WebhookMalformed,
    /// The Discord webhook URL scheme was not `https`
    /// ([`EgressPolicyError::NotHttps`]).
    #[error("discord webhook url: must use https")]
    DiscordNotHttps,
    /// The Discord webhook URL had no host
    /// ([`EgressPolicyError::MissingHost`]).
    #[error("discord webhook url: has no host")]
    DiscordMissingHost,
    /// The Discord webhook URL is a bare IP-literal in a denied range
    /// ([`EgressPolicyError::BlockedAddress`]).
    #[error("discord webhook url: resolves to a blocked address")]
    DiscordBlockedAddress,
    /// The Discord webhook URL did not parse
    /// ([`EgressPolicyError::MalformedUrl`]).
    #[error("discord webhook url: is malformed")]
    DiscordMalformed,
}

/// Maps an egress-policy refusal to the DISCORD-field error variants.
///
/// Deliberately not a second `From` impl: `From<EgressPolicyError>` is already
/// taken by the generic-webhook variants and `?` would then silently name the
/// wrong field in the problem+json `detail`. Naming the mapper at the call site
/// makes the field explicit.
fn discord_policy_error(err: EgressPolicyError) -> DeliveryConfigError {
    match err {
        EgressPolicyError::NotHttps => DeliveryConfigError::DiscordNotHttps,
        EgressPolicyError::MissingHost => DeliveryConfigError::DiscordMissingHost,
        EgressPolicyError::BlockedAddress => DeliveryConfigError::DiscordBlockedAddress,
        EgressPolicyError::MalformedUrl => DeliveryConfigError::DiscordMalformed,
    }
}

/// Validates one optional user-supplied egress URL field: `None` or a blank
/// string clears it, anything else is trimmed and put through
/// [`validate_egress_url`], with `on_refusal` naming the field in the error.
///
/// Shared by the two URL fields ON PURPOSE rather than for brevity: the
/// clearing semantics ("absent and blank both mean cleared") must be IDENTICAL
/// for both, because the HTTP PUT is a replace and a divergence would make one
/// field clearable and the other not.
fn parse_optional_egress_url(
    raw: Option<String>,
    on_refusal: fn(EgressPolicyError) -> DeliveryConfigError,
) -> Result<Option<String>, DeliveryConfigError> {
    match raw {
        None => Ok(None),
        Some(ref url) if url.trim().is_empty() => Ok(None),
        Some(url) => {
            let trimmed = url.trim();
            validate_egress_url(trimmed).map_err(on_refusal)?;
            Ok(Some(trimmed.to_owned()))
        }
    }
}

impl From<EgressPolicyError> for DeliveryConfigError {
    /// Maps each static egress-policy refusal to its own field-named delivery
    /// error, so the HTTP `detail` states a distinct reason per cause.
    fn from(err: EgressPolicyError) -> Self {
        match err {
            EgressPolicyError::NotHttps => Self::WebhookNotHttps,
            EgressPolicyError::MissingHost => Self::WebhookMissingHost,
            EgressPolicyError::BlockedAddress => Self::WebhookBlockedAddress,
            EgressPolicyError::MalformedUrl => Self::WebhookMalformed,
        }
    }
}

/// Minimal, no-regex structural email check (the hand-rolled callsign/grid/
/// frequency domain posture): exactly one `@`, a non-empty local part and
/// domain, and no whitespace anywhere. `normalize_email` only trims/lowercases,
/// and there is no existing strict email validator to reuse, so this small
/// check lives here. It is deliberately permissive on the domain shape — the
/// authoritative "is this mailbox reachable" answer is a delivery-time concern
/// (`Mailer`), not a config-time gate.
fn is_structurally_valid_email(addr: &str) -> bool {
    if addr.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let mut parts = addr.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty() && !domain.is_empty()
}

/// Validates a raw delivery config into the typed write shape.
///
/// - **Emails**: each entry is trimmed; blank entries are dropped; the survivors
///   are bounded in length, normalized ([`normalize_email`]), structurally
///   checked, and the total count is bounded by [`MAX_DELIVERY_EMAILS`]. Two
///   entries that normalize to the same address (including pure case
///   variants, e.g. `Foo@example.com` / `foo@example.com`) collapse to ONE — a duplicate
///   target would otherwise send the same net's summary twice
///   to the same mailbox. An empty result is valid ("no email targets").
/// - **Webhook URL** and **Discord webhook URL**: `None`/blank clears either;
///   otherwise each is trimmed, validated by [`validate_egress_url`] and stored
///   on success. Both go through the SAME check — a Discord URL gets no host
///   exemption of any kind.
///
/// The webhook check here is the CONFIG-TIME gate only — it is pure
/// and does NO DNS, so it catches scheme/syntax and bare-IP-literal problems
/// but nothing a hostname resolves to. A host safe at save time can be
/// DNS-rebound before the next delivery, so it is **necessary but not
/// sufficient**: delivery MUST still re-validate live through the
/// `SsrfSafeEgress` resolver-pin. Passing here is never a durable guarantee.
pub fn parse_delivery_config(
    raw: RawDeliveryConfig,
) -> Result<DeliveryConfigFields, DeliveryConfigError> {
    let non_blank: Vec<&str> = raw
        .emails
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .collect();

    if non_blank.len() > MAX_DELIVERY_EMAILS {
        return Err(DeliveryConfigError::TooManyEmails);
    }

    let mut emails = Vec::with_capacity(non_blank.len());
    let mut seen = std::collections::HashSet::with_capacity(non_blank.len());
    for entry in non_blank {
        let bounded =
            parse_bounded_text(entry, MAX_EMAIL_CHARS).map_err(DeliveryConfigError::Email)?;
        let normalized = normalize_email(&bounded);
        if !is_structurally_valid_email(&normalized) {
            return Err(DeliveryConfigError::EmailMalformed);
        }
        // Collapse duplicates (post-normalization) so the same address is
        // never targeted twice by one config.
        if seen.insert(normalized.clone()) {
            emails.push(normalized);
        }
    }

    let webhook_url = parse_optional_egress_url(raw.webhook_url, DeliveryConfigError::from)?;
    let discord_webhook_url =
        parse_optional_egress_url(raw.discord_webhook_url, discord_policy_error)?;

    Ok(DeliveryConfigFields {
        emails,
        webhook_url,
        discord_webhook_url,
    })
}

/// HMAC-SHA256 keyed by the raw webhook-secret bytes over the exact body bytes.
type WebhookMac = Hmac<Sha256>;

/// Signs the exact webhook body bytes with the per-net secret, returning the
/// GitHub-convention `"sha256=<lowercase-hex>"` signature.
///
/// Pure computation, no I/O: HMAC-SHA256 with `key = secret` bytes and
/// `msg = body` (the EXACT serialized JSON bytes sent as the POST body). The caller
/// serializes the payload ONCE and signs those same bytes, so a receiver
/// verifies by recomputing `HMAC-SHA256(secret, rawBody)` and constant-time
/// comparing the hex. The `sha256=` prefix names the algorithm for
/// forward-compatibility (`X-Hub-Signature-256` convention). `secret` is the
/// stored per-net webhook secret READ at delivery time — this function never
/// mints or rotates it.
pub fn sign_webhook(secret: &str, body: &[u8]) -> String {
    // HMAC accepts a key of any length, so `new_from_slice` never errors here.
    let mut mac = WebhookMac::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts a key of any length");
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    let hex: String = tag.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256={hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independently recomputes the expected signature with the `hmac`/`sha2`
    /// primitives directly, so the assertion pins LOGIC (a keyed hash the
    /// receiver can reproduce), not the function's own output echoed back.
    fn independent_sig(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
        mac.update(body);
        let tag = mac.finalize().into_bytes();
        let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
        format!("sha256={hex}")
    }

    #[test]
    fn sign_webhook_matches_an_independently_computed_hmac_and_is_lowercase_hex() {
        let secret = "s3cr3t-webhook-key";
        let body = br#"{"net":"Sunday Traffic","participantCount":3}"#;
        let sig = sign_webhook(secret, body);
        assert_eq!(sig, independent_sig(secret, body));
        let hex = sig
            .strip_prefix("sha256=")
            .expect("carries the sha256= algorithm prefix");
        assert_eq!(hex.len(), 64, "sha256 digest is 32 bytes = 64 hex chars");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hex is lowercase"
        );
    }

    #[test]
    fn sign_webhook_matches_a_known_answer_hmac_sha256_vector() {
        // Well-known fixed vector: HMAC-SHA256(key="key",
        // msg="The quick brown fox jumps over the lazy dog").
        let sig = sign_webhook("key", b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            sig,
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn sign_webhook_is_deterministic_for_the_same_inputs() {
        assert_eq!(sign_webhook("k", b"body"), sign_webhook("k", b"body"));
    }

    #[test]
    fn a_one_byte_body_change_flips_the_signature() {
        assert_ne!(
            sign_webhook("k", b"payloadA"),
            sign_webhook("k", b"payloadB")
        );
    }

    #[test]
    fn a_different_secret_flips_the_signature() {
        assert_ne!(sign_webhook("k1", b"body"), sign_webhook("k2", b"body"));
    }

    #[test]
    fn a_valid_config_parses_to_the_typed_shape_with_normalized_emails() {
        let fields = parse_delivery_config(RawDeliveryConfig {
            emails: vec![
                "Alice@Example.com".to_owned(),
                " bob@example.org ".to_owned(),
            ],
            webhook_url: Some("https://hooks.example.com/net".to_owned()),
            discord_webhook_url: None,
        })
        .expect("a couple of emails + an https hostname webhook is valid");
        assert_eq!(
            fields.emails,
            vec!["alice@example.com".to_owned(), "bob@example.org".to_owned()],
            "addresses are trimmed + lowercased (normalize_email)"
        );
        assert_eq!(
            fields.webhook_url.as_deref(),
            Some("https://hooks.example.com/net")
        );
        assert!(!fields.is_delivery_off());
    }

    #[test]
    fn an_empty_config_is_valid_and_is_delivery_off() {
        let fields = parse_delivery_config(RawDeliveryConfig::default())
            .expect("no emails and no webhook is a valid config");
        assert!(fields.emails.is_empty());
        assert!(fields.webhook_url.is_none());
        assert!(
            fields.is_delivery_off(),
            "no targets means delivery is armed OFF"
        );
    }

    #[test]
    fn duplicate_emails_collapse_to_one_even_across_case() {
        let fields = parse_delivery_config(RawDeliveryConfig {
            emails: vec![
                "alerts@example.com".to_owned(),
                "Alerts@Example.com".to_owned(),
                " ALERTS@EXAMPLE.COM ".to_owned(),
                "other@example.com".to_owned(),
            ],
            webhook_url: None,
            discord_webhook_url: None,
        })
        .expect("duplicates are not a rejection, just collapsed");
        assert_eq!(
            fields.emails,
            vec![
                "alerts@example.com".to_owned(),
                "other@example.com".to_owned()
            ],
            "case-variant duplicates collapse to a single normalized entry, order preserved"
        );
    }

    #[test]
    fn blank_email_entries_are_dropped() {
        let fields = parse_delivery_config(RawDeliveryConfig {
            emails: vec!["".to_owned(), "  ".to_owned(), "a@example.com".to_owned()],
            webhook_url: None,
            discord_webhook_url: None,
        })
        .expect("blanks drop, one real address remains");
        assert_eq!(fields.emails, vec!["a@example.com".to_owned()]);
    }

    #[test]
    fn each_egress_policy_reason_maps_to_a_distinct_webhook_error() {
        let mapped = |e: EgressPolicyError| DeliveryConfigError::from(e).to_string();
        let not_https = mapped(EgressPolicyError::NotHttps);
        let missing = mapped(EgressPolicyError::MissingHost);
        let blocked = mapped(EgressPolicyError::BlockedAddress);
        let malformed = mapped(EgressPolicyError::MalformedUrl);

        // Each is field-prefixed with "webhook url:".
        for detail in [&not_https, &missing, &blocked, &malformed] {
            assert!(
                detail.starts_with("webhook url:"),
                "detail is field-named: {detail}"
            );
        }
        // Every reason is a DISTINCT message (no two collide).
        let set: std::collections::HashSet<_> = [&not_https, &missing, &blocked, &malformed]
            .into_iter()
            .collect();
        assert_eq!(set.len(), 4, "four reasons produce four distinct details");
    }

    #[test]
    fn a_non_https_webhook_is_rejected_distinctly_from_a_blocked_one() {
        let not_https = parse_delivery_config(RawDeliveryConfig {
            emails: vec![],
            webhook_url: Some("http://example.com/hook".to_owned()),
            discord_webhook_url: None,
        })
        .expect_err("http:// is refused");
        let blocked = parse_delivery_config(RawDeliveryConfig {
            emails: vec![],
            webhook_url: Some("https://169.254.169.254/latest/meta-data/".to_owned()),
            discord_webhook_url: None,
        })
        .expect_err("a metadata-address literal is refused");
        assert_eq!(not_https, DeliveryConfigError::WebhookNotHttps);
        assert_eq!(blocked, DeliveryConfigError::WebhookBlockedAddress);
        assert_ne!(not_https.to_string(), blocked.to_string());
    }

    #[test]
    fn a_malformed_webhook_url_is_rejected() {
        let err = parse_delivery_config(RawDeliveryConfig {
            emails: vec![],
            webhook_url: Some("not a url".to_owned()),
            discord_webhook_url: None,
        })
        .expect_err("a non-URL string is refused");
        assert_eq!(err, DeliveryConfigError::WebhookMalformed);
    }

    #[test]
    fn over_the_email_count_limit_is_a_field_named_rejection() {
        let too_many: Vec<String> = (0..=MAX_DELIVERY_EMAILS)
            .map(|i| format!("user{i}@example.com"))
            .collect();
        let err = parse_delivery_config(RawDeliveryConfig {
            emails: too_many,
            webhook_url: None,
            discord_webhook_url: None,
        })
        .expect_err("more than the max is refused");
        assert_eq!(err, DeliveryConfigError::TooManyEmails);
        assert!(err.to_string().starts_with("delivery emails:"));
    }

    #[test]
    fn a_malformed_email_is_a_field_named_rejection() {
        for bad in ["no-at-sign", "two@@ats.com", "@nodomain.com", "nolocal@"] {
            let err = parse_delivery_config(RawDeliveryConfig {
                emails: vec![bad.to_owned()],
                webhook_url: None,
                discord_webhook_url: None,
            })
            .unwrap_err();
            assert_eq!(err, DeliveryConfigError::EmailMalformed, "case: {bad}");
            assert!(err.to_string().starts_with("delivery email:"));
        }
    }

    #[test]
    fn an_over_length_email_is_a_field_named_bounded_text_rejection() {
        let long_local = "a".repeat(300);
        let err = parse_delivery_config(RawDeliveryConfig {
            emails: vec![format!("{long_local}@example.com")],
            webhook_url: None,
            discord_webhook_url: None,
        })
        .expect_err("an over-length address is refused");
        assert!(
            matches!(err, DeliveryConfigError::Email(_)),
            "bounded-text failure surfaces as the Email variant"
        );
        assert!(err.to_string().starts_with("delivery email:"));
    }

    #[test]
    fn distinct_fields_carry_distinct_detail_text() {
        let bad_email = parse_delivery_config(RawDeliveryConfig {
            emails: vec!["bogus".to_owned()],
            webhook_url: None,
            discord_webhook_url: None,
        })
        .expect_err("bad email");
        let bad_webhook = parse_delivery_config(RawDeliveryConfig {
            emails: vec![],
            webhook_url: Some("http://x.example/".to_owned()),
            discord_webhook_url: None,
        })
        .expect_err("bad webhook");
        assert_ne!(bad_email.to_string(), bad_webhook.to_string());
        assert!(bad_email.to_string().starts_with("delivery email:"));
        assert!(bad_webhook.to_string().starts_with("webhook url:"));
    }
    #[test]
    fn a_discord_webhook_url_parses_and_is_cleared_by_a_blank() {
        let fields = parse_delivery_config(RawDeliveryConfig {
            emails: Vec::new(),
            webhook_url: None,
            discord_webhook_url: Some("  https://discord.com/api/webhooks/12/tok  ".to_owned()),
        })
        .expect("an https Discord webhook URL is valid");
        assert_eq!(
            fields.discord_webhook_url.as_deref(),
            Some("https://discord.com/api/webhooks/12/tok"),
            "the stored URL is trimmed but otherwise VERBATIM — it is the owner's credential"
        );

        let cleared = parse_delivery_config(RawDeliveryConfig {
            emails: Vec::new(),
            webhook_url: None,
            discord_webhook_url: Some("   ".to_owned()),
        })
        .expect("a blank clears");
        assert!(cleared.discord_webhook_url.is_none());
    }

    #[test]
    fn a_refused_discord_url_names_the_discord_field_and_never_echoes_the_url() {
        // Same egress policy as the generic webhook, no host exemption,
        // but a DISTINCT field name in the problem+json detail.
        let cases = [
            (
                "http://discord.com/api/webhooks/12/tok",
                DeliveryConfigError::DiscordNotHttps,
            ),
            (
                "https://169.254.169.254/api/webhooks/12/tok",
                DeliveryConfigError::DiscordBlockedAddress,
            ),
            ("not a url", DeliveryConfigError::DiscordMalformed),
        ];
        for (url, expected) in cases {
            let err = parse_delivery_config(RawDeliveryConfig {
                emails: Vec::new(),
                webhook_url: None,
                discord_webhook_url: Some(url.to_owned()),
            })
            .expect_err("the URL is refused");
            assert_eq!(err, expected);
            let detail = err.to_string();
            assert!(
                detail.starts_with("discord webhook url: "),
                "the detail names the DISCORD field, not the generic webhook: {detail}"
            );
            assert!(
                !detail.contains("discord.com")
                    && !detail.contains("tok")
                    && !detail.contains("169.254"),
                "no variant echoes the submitted URL: {detail}"
            );
        }
    }

    #[test]
    fn a_discord_only_config_is_not_delivery_off() {
        let discord_only = DeliveryConfigFields {
            emails: Vec::new(),
            webhook_url: None,
            discord_webhook_url: Some("https://discord.com/api/webhooks/12/tok".to_owned()),
        };
        assert!(
            !discord_only.is_delivery_off(),
            "a net whose ONLY target is Discord has delivery ON"
        );
        let nothing = DeliveryConfigFields {
            emails: Vec::new(),
            webhook_url: None,
            discord_webhook_url: None,
        };
        assert!(nothing.is_delivery_off());
    }
}
