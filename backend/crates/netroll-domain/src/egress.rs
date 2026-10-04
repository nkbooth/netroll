// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! SSRF egress policy. A user-supplied URL can point at an internal address the
//! server reaches and the attacker cannot: cloud metadata, loopback services,
//! RFC-1918 hosts. This is the PURE half of the defence — a deny-CIDR table and
//! a URL-policy validator, no I/O and no DNS. The resolution-time half pins
//! validated IPs in the adapters crate, and BOTH are required.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ip_network::{Ipv4Network, Ipv6Network};
use thiserror::Error;
use url::{Host, Url};

use crate::ports::BoxFuture;

/// Deny-listed IPv4 CIDR ranges: `(network address, prefix length)`.
///
/// This explicit, commented table is the **security control of record** — a
/// reviewer can read exactly what is refused. A defense-in-depth backstop
/// (`!is_global`) in [`is_blocked_ip`] additionally refuses anything not
/// globally routable, so an unlisted-but-non-public address still fails closed.
const BLOCKED_IPV4: &[(Ipv4Addr, u8)] = &[
    (Ipv4Addr::new(0, 0, 0, 0), 8), // "this network" / unspecified (RFC 1122)
    (Ipv4Addr::new(10, 0, 0, 0), 8), // RFC-1918 private
    (Ipv4Addr::new(100, 64, 0, 0), 10), // CGNAT / shared address space (RFC 6598)
    (Ipv4Addr::new(127, 0, 0, 0), 8), // loopback
    (Ipv4Addr::new(169, 254, 0, 0), 16), // link-local — contains cloud metadata 169.254.169.254
    (Ipv4Addr::new(172, 16, 0, 0), 12), // RFC-1918 private
    (Ipv4Addr::new(192, 0, 0, 0), 24), // IETF protocol assignments
    (Ipv4Addr::new(192, 0, 2, 0), 24), // TEST-NET-1 documentation
    (Ipv4Addr::new(192, 88, 99, 0), 24), // 6to4 anycast relay
    (Ipv4Addr::new(192, 168, 0, 0), 16), // RFC-1918 private
    (Ipv4Addr::new(198, 18, 0, 0), 15), // benchmarking (RFC 2544)
    (Ipv4Addr::new(198, 51, 100, 0), 24), // TEST-NET-2 documentation
    (Ipv4Addr::new(203, 0, 113, 0), 24), // TEST-NET-3 documentation
    (Ipv4Addr::new(224, 0, 0, 0), 4), // multicast
    (Ipv4Addr::new(240, 0, 0, 0), 4), // reserved (former class E)
    (Ipv4Addr::new(255, 255, 255, 255), 32), // limited broadcast
];

/// Deny-listed IPv6 CIDR ranges: `(network address, prefix length)`.
///
/// The IPv4-mapped range (`::ffff:0:0/96`) is deliberately **absent** — a
/// mapped address is unwrapped to its embedded IPv4 and classified against
/// [`BLOCKED_IPV4`] instead, so a mapped *global* address still works while
/// mapped loopback/metadata is refused. See [`is_blocked_ip`].
const BLOCKED_IPV6: &[(Ipv6Addr, u8)] = &[
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0), 128), // unspecified (::)
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1), 128), // loopback (::1)
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7), // unique local address (ULA)
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10), // link-local unicast
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8), // multicast
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32), // documentation
];

/// Whether `ip` must be refused as an SSRF egress target.
///
/// Returns `true` for any address that is loopback, link-local (including the
/// `169.254.169.254` cloud-metadata endpoint), RFC-1918 private, CGNAT/shared,
/// unspecified, broadcast, multicast, ULA, documentation, or otherwise not a
/// globally-routable public address — and for every IPv6 encoding that embeds
/// an IPv4 address: **IPv4-mapped / IPv4-compatible** (`::ffff:127.0.0.1`,
/// `::ffff:169.254.169.254`), **6to4** (`2002:a9fe:a9fe::`, RFC 3056), and
/// **NAT64** (`64:ff9b::a9fe:a9fe`, RFC 6052). Each embedding is unwrapped and
/// re-checked against the IPv4 rules so none of them can bypass the filter.
///
/// Fails closed: the explicit [`BLOCKED_IPV4`]/[`BLOCKED_IPV6`] tables are the
/// primary control; a `!is_global` backstop refuses anything not explicitly
/// listed but also not publicly routable.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d) forms
            // MUST be unwrapped and classified against the IPv4 rules — this is
            // the #1 real-world SSRF filter bypass (`::ffff:169.254.169.254`).
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ipv4(mapped);
            }
            if let Some(compat) = v6.to_ipv4() {
                return is_blocked_ipv4(compat);
            }
            // 6to4 (RFC 3056, `2002::/16`) and the NAT64 well-known prefix
            // (RFC 6052, `64:ff9b::/96`) are two MORE forms that embed an IPv4
            // address in an IPv6 literal, exactly like the mapped/compatible
            // forms above — and both fall inside `is_unicast_global()`'s
            // "globally routable" definition (`ip_network` does not special-case
            // them), so an address like `2002:a9fe:a9fe::` or
            // `64:ff9b::a9fe:a9fe` (both encode `169.254.169.254`) would
            // otherwise sail past both the explicit table and the `!is_global`
            // backstop. Unwrap and re-check them the same way.
            if let Some(embedded) = to_ipv4_6to4(v6) {
                return is_blocked_ipv4(embedded);
            }
            if let Some(embedded) = to_ipv4_nat64(v6) {
                return is_blocked_ipv4(embedded);
            }
            is_blocked_ipv6(v6)
        }
    }
}

/// Extracts the IPv4 address embedded in a 6to4 (RFC 3056, `2002::/16`)
/// address: the 32 bits immediately after the `2002` prefix (segments 1 and 2)
/// carry the embedded IPv4 octets.
fn to_ipv4_6to4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = v6.segments();
    (s[0] == 0x2002).then(|| Ipv4Addr::from(((s[1] as u32) << 16) | s[2] as u32))
}

/// Extracts the IPv4 address embedded in a NAT64 well-known prefix (RFC 6052,
/// `64:ff9b::/96`) address: the trailing 32 bits (segments 6 and 7) carry the
/// embedded IPv4 octets.
fn to_ipv4_nat64(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = v6.segments();
    (s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0)
        .then(|| Ipv4Addr::from(((s[6] as u32) << 16) | s[7] as u32))
}

/// Classifies a bare IPv4 address against the deny table plus the `!is_global`
/// fail-closed backstop.
fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    if BLOCKED_IPV4.iter().any(|(network, prefix)| {
        Ipv4Network::new_truncate(*network, *prefix)
            .expect("deny-table IPv4 prefixes are valid")
            .contains(ip)
    }) {
        return true;
    }
    // Backstop: refuse anything not globally routable (e.g. an IANA range added
    // after this code was written). The /32 host network carries the address.
    !Ipv4Network::new(ip, 32)
        .expect("a /32 is always a valid IPv4 network")
        .is_global()
}

/// Classifies a bare IPv6 address against the deny table plus the `!is_global`
/// fail-closed backstop.
fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    if BLOCKED_IPV6.iter().any(|(network, prefix)| {
        Ipv6Network::new_truncate(*network, *prefix)
            .expect("deny-table IPv6 prefixes are valid")
            .contains(ip)
    }) {
        return true;
    }
    !Ipv6Network::new(ip, 128)
        .expect("a /128 is always a valid IPv6 network")
        .is_global()
}

/// Why a URL was refused by the static egress policy ([`validate_egress_url`]).
///
/// `Display` text is deliberately fixed and never echoes the caller-supplied
/// URL or host — unbounded/untrusted input must not reach logs.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EgressPolicyError {
    /// The scheme was not `https` (an `http://`, `file://`, … URL).
    #[error("egress URL must use https")]
    NotHttps,
    /// The URL had no host component.
    #[error("egress URL has no host")]
    MissingHost,
    /// The host is an IP literal in a denied (non-public) range.
    #[error("egress URL resolves to a blocked address")]
    BlockedAddress,
    /// The string did not parse as a URL.
    #[error("egress URL is malformed")]
    MalformedUrl,
}

/// Statically validates a user-supplied egress URL without any network I/O.
///
/// Enforces `https`-only, requires a host, and — for a **bare IP-literal host**
/// — refuses any address in a denied range via [`is_blocked_ip`]. A hostname
/// host passes this syntactic gate: its resolved addresses can only be checked
/// at delivery time, which the SSRF-safe adapter's DNS resolver does. This is
/// the config-time "reject obviously-bad URLs" check; the
/// authoritative control remains delivery-time resolution.
pub fn validate_egress_url(raw: &str) -> Result<(), EgressPolicyError> {
    let url = Url::parse(raw).map_err(|_| EgressPolicyError::MalformedUrl)?;

    if url.scheme() != "https" {
        return Err(EgressPolicyError::NotHttps);
    }

    match url.host() {
        None => Err(EgressPolicyError::MissingHost),
        Some(Host::Domain("")) => Err(EgressPolicyError::MissingHost),
        Some(Host::Domain(_)) => Ok(()),
        Some(Host::Ipv4(addr)) => reject_if_blocked(IpAddr::V4(addr)),
        Some(Host::Ipv6(addr)) => reject_if_blocked(IpAddr::V6(addr)),
    }
}

/// Maps a blocked IP-literal host to [`EgressPolicyError::BlockedAddress`].
fn reject_if_blocked(ip: IpAddr) -> Result<(), EgressPolicyError> {
    if is_blocked_ip(ip) {
        Err(EgressPolicyError::BlockedAddress)
    } else {
        Ok(())
    }
}

/// HTTP method the egress client will issue. Only the two verbs the known
/// consumers need (POST, GET) — additive later (YAGNI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressMethod {
    /// HTTP GET.
    Get,
    /// HTTP POST.
    Post,
}

/// An outbound request to send through the SSRF-safe egress client.
///
/// `Debug` is hand-written (not derived) to redact the query string of `url`:
/// some callers embed a credential directly in the query string (the QRZ
/// session-key login is `?username=&password=&agent=`), and that value must
/// never appear in any log output — including an accidental future
/// `tracing::debug!(?req)` on a value built with one. Scheme/host/path stay
/// visible since that's the useful part for non-secret callers, such as the
/// webhook POST.
#[derive(Clone)]
pub struct EgressRequest {
    /// The HTTP verb.
    pub method: EgressMethod,
    /// The absolute target URL (validated `https://` at send time).
    pub url: String,
    /// Request headers, e.g. the HMAC signature header.
    pub headers: Vec<(String, String)>,
    /// Optional request body.
    pub body: Option<Vec<u8>>,
}

impl core::fmt::Debug for EgressRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let redacted_url = match self.url.split_once('?') {
            Some((base, _)) => format!("{base}?<redacted>"),
            None => self.url.clone(),
        };
        f.debug_struct("EgressRequest")
            .field("method", &self.method)
            .field("url", &redacted_url)
            .field("headers", &self.headers)
            .field(
                "body",
                &self.body.as_ref().map(|b| format!("<{} bytes>", b.len())),
            )
            .finish()
    }
}

/// The response from a completed egress request. `body` is already size-capped
/// by the adapter's streamed read.
#[derive(Debug, Clone)]
pub struct EgressResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body, already bounded to the adapter's size cap.
    pub body: Vec<u8>,
}

/// Why an egress request failed. Every variant is a refusal or transport fault
/// that must propagate to the caller — never swallowed.
///
/// `Transport` text is bounded by the adapter and must not leak internal
/// network detail or unbounded user input beyond what the caller needs.
#[derive(Debug, Error)]
pub enum EgressError {
    /// The target (or a resolved/redirected address) is in a denied range.
    #[error("egress target is a blocked address")]
    BlockedAddress,
    /// The URL was malformed or otherwise unusable.
    #[error("egress URL is invalid")]
    InvalidUrl,
    /// The scheme was not `https` (initial request or a redirect `Location`).
    #[error("egress URL must use https")]
    NotHttps,
    /// The redirect chain exceeded the bounded hop count.
    #[error("egress request exceeded the redirect limit")]
    TooManyRedirects,
    /// The response body exceeded the maximum allowed size.
    #[error("egress response exceeded the size limit")]
    ResponseTooLarge,
    /// The connect or total-request timeout elapsed.
    #[error("egress request timed out")]
    Timeout,
    /// A transport-level failure. The message is bounded by the adapter.
    #[error("egress transport error: {0}")]
    Transport(String),
}

impl From<EgressPolicyError> for EgressError {
    /// Lifts a static policy refusal into the runtime egress error type so the
    /// adapter's up-front URL check reports one consistent error to callers.
    fn from(err: EgressPolicyError) -> Self {
        match err {
            EgressPolicyError::NotHttps => EgressError::NotHttps,
            EgressPolicyError::BlockedAddress => EgressError::BlockedAddress,
            EgressPolicyError::MissingHost | EgressPolicyError::MalformedUrl => {
                EgressError::InvalidUrl
            }
        }
    }
}

/// Centralized SSRF-safe outbound HTTP port.
///
/// The single chokepoint every user-URL-driven outbound request funnels
/// through: webhook delivery and QRZ lookups. The
/// implementation enforces `https`-only, resolve-then-pin IP validation,
/// bounded re-validated redirects, timeouts, and a response-size cap.
pub trait Egress {
    /// Sends `req` through the SSRF-safe client; errors on any policy refusal
    /// or transport fault.
    fn send<'a>(&'a self, req: EgressRequest)
    -> BoxFuture<'a, Result<EgressResponse, EgressError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).expect("test address parses")
    }

    // --- One representative address per denied IPv4 range ---

    #[test]
    fn ipv4_loopback_is_blocked() {
        assert!(is_blocked_ip(ip("127.0.0.1")));
    }

    #[test]
    fn ipv4_link_local_is_blocked() {
        assert!(is_blocked_ip(ip("169.254.1.1")));
    }

    #[test]
    fn ipv4_cloud_metadata_address_is_blocked() {
        // The single most common SSRF target: cloud instance metadata.
        assert!(is_blocked_ip(ip("169.254.169.254")));
    }

    #[test]
    fn ipv4_rfc1918_ten_is_blocked() {
        assert!(is_blocked_ip(ip("10.0.0.5")));
    }

    #[test]
    fn ipv4_rfc1918_172_is_blocked() {
        assert!(is_blocked_ip(ip("172.16.5.5")));
    }

    #[test]
    fn ipv4_rfc1918_192_168_is_blocked() {
        assert!(is_blocked_ip(ip("192.168.1.1")));
    }

    #[test]
    fn ipv4_cgnat_shared_is_blocked() {
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
    }

    #[test]
    fn ipv4_unspecified_is_blocked() {
        assert!(is_blocked_ip(ip("0.0.0.0")));
    }

    #[test]
    fn ipv4_broadcast_is_blocked() {
        assert!(is_blocked_ip(ip("255.255.255.255")));
    }

    #[test]
    fn ipv4_multicast_is_blocked() {
        assert!(is_blocked_ip(ip("224.0.0.1")));
    }

    #[test]
    fn ipv4_reserved_is_blocked() {
        assert!(is_blocked_ip(ip("240.0.0.1")));
    }

    #[test]
    fn ipv4_ietf_protocol_is_blocked() {
        assert!(is_blocked_ip(ip("192.0.0.1")));
    }

    #[test]
    fn ipv4_test_net_1_documentation_is_blocked() {
        assert!(is_blocked_ip(ip("192.0.2.1")));
    }

    #[test]
    fn ipv4_test_net_2_documentation_is_blocked() {
        assert!(is_blocked_ip(ip("198.51.100.1")));
    }

    #[test]
    fn ipv4_test_net_3_documentation_is_blocked() {
        assert!(is_blocked_ip(ip("203.0.113.1")));
    }

    #[test]
    fn ipv4_benchmarking_is_blocked() {
        assert!(is_blocked_ip(ip("198.18.0.1")));
    }

    #[test]
    fn ipv4_6to4_anycast_relay_is_blocked() {
        assert!(is_blocked_ip(ip("192.88.99.1")));
    }

    // --- One representative address per denied IPv6 range ---

    #[test]
    fn ipv6_loopback_is_blocked() {
        assert!(is_blocked_ip(ip("::1")));
    }

    #[test]
    fn ipv6_unspecified_is_blocked() {
        assert!(is_blocked_ip(ip("::")));
    }

    #[test]
    fn ipv6_ula_is_blocked() {
        assert!(is_blocked_ip(ip("fc00::1")));
        assert!(is_blocked_ip(ip("fd12:3456::1")));
    }

    #[test]
    fn ipv6_link_local_is_blocked() {
        assert!(is_blocked_ip(ip("fe80::1")));
    }

    #[test]
    fn ipv6_multicast_is_blocked() {
        assert!(is_blocked_ip(ip("ff02::1")));
    }

    #[test]
    fn ipv6_documentation_is_blocked() {
        assert!(is_blocked_ip(ip("2001:db8::1")));
    }

    // --- The IPv4-mapped / compatible IPv6 bypass (top-priority landmine) ---

    #[test]
    fn ipv4_mapped_loopback_is_blocked() {
        assert!(is_blocked_ip(ip("::ffff:127.0.0.1")));
    }

    #[test]
    fn ipv4_mapped_cloud_metadata_is_blocked() {
        assert!(is_blocked_ip(ip("::ffff:169.254.169.254")));
    }

    #[test]
    fn ipv4_mapped_private_is_blocked() {
        assert!(is_blocked_ip(ip("::ffff:10.0.0.1")));
    }

    #[test]
    fn ipv4_mapped_global_is_allowed() {
        // A mapped *global* address must still pass — do not block the whole
        // ::ffff:0:0/96 block; unwrap and classify the embedded IPv4.
        assert!(!is_blocked_ip(ip("::ffff:93.184.216.34")));
    }

    // --- The 6to4 / NAT64 embedded-IPv4 bypass (adversarial-review finding) ---

    #[test]
    fn ipv6_6to4_encoded_cloud_metadata_is_blocked() {
        // 2002:a9fe:a9fe:: is the 6to4 (RFC 3056) encoding of 169.254.169.254.
        assert!(is_blocked_ip(ip("2002:a9fe:a9fe::")));
    }

    #[test]
    fn ipv6_6to4_encoded_loopback_is_blocked() {
        // 2002:7f00:1:: is the 6to4 encoding of 127.0.0.1.
        assert!(is_blocked_ip(ip("2002:7f00:1::")));
    }

    #[test]
    fn ipv6_6to4_encoded_global_is_allowed() {
        // A 6to4 address embedding a *global* IPv4 must still pass.
        assert!(!is_blocked_ip(ip("2002:5db8:d822::")));
    }

    #[test]
    fn ipv6_nat64_encoded_cloud_metadata_is_blocked() {
        // 64:ff9b::a9fe:a9fe is the NAT64 well-known-prefix (RFC 6052) encoding
        // of 169.254.169.254.
        assert!(is_blocked_ip(ip("64:ff9b::a9fe:a9fe")));
    }

    #[test]
    fn ipv6_nat64_encoded_private_is_blocked() {
        // 64:ff9b::a00:1 is the NAT64 encoding of 10.0.0.1.
        assert!(is_blocked_ip(ip("64:ff9b::a00:1")));
    }

    #[test]
    fn ipv6_nat64_encoded_global_is_allowed() {
        // A NAT64 address embedding a *global* IPv4 must still pass.
        assert!(!is_blocked_ip(ip("64:ff9b::5db8:d822")));
    }

    // --- Known-global addresses pass ---

    #[test]
    fn global_ipv4_is_allowed() {
        assert!(!is_blocked_ip(ip("93.184.216.34")));
    }

    #[test]
    fn global_ipv6_is_allowed() {
        assert!(!is_blocked_ip(ip("2606:2800:220:1:248:1893:25c8:1946")));
    }

    // --- Validate_egress_url ---

    #[test]
    fn http_scheme_is_rejected() {
        assert_eq!(
            validate_egress_url("http://example.com/hook"),
            Err(EgressPolicyError::NotHttps)
        );
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        assert_eq!(
            validate_egress_url("file:///etc/passwd"),
            Err(EgressPolicyError::NotHttps)
        );
        assert_eq!(
            validate_egress_url("ftp://example.com/x"),
            Err(EgressPolicyError::NotHttps)
        );
    }

    #[test]
    fn scheme_less_string_is_malformed() {
        assert_eq!(
            validate_egress_url("example.com/hook"),
            Err(EgressPolicyError::MalformedUrl)
        );
    }

    #[test]
    fn https_with_denied_ipv4_literal_is_rejected() {
        assert_eq!(
            validate_egress_url("https://169.254.169.254/latest/meta-data/"),
            Err(EgressPolicyError::BlockedAddress)
        );
    }

    #[test]
    fn https_with_denied_ipv6_literal_is_rejected() {
        assert_eq!(
            validate_egress_url("https://[::1]/"),
            Err(EgressPolicyError::BlockedAddress)
        );
    }

    #[test]
    fn https_with_ipv4_mapped_loopback_literal_is_rejected() {
        assert_eq!(
            validate_egress_url("https://[::ffff:127.0.0.1]/"),
            Err(EgressPolicyError::BlockedAddress)
        );
    }

    #[test]
    fn https_with_ordinary_hostname_is_accepted() {
        assert_eq!(validate_egress_url("https://example.com/hook"), Ok(()));
    }

    #[test]
    fn https_with_global_ip_literal_is_accepted() {
        assert_eq!(validate_egress_url("https://93.184.216.34/"), Ok(()));
    }

    #[test]
    fn policy_error_display_does_not_echo_the_input() {
        // The fixed message must not carry the untrusted URL/host back.
        let err = validate_egress_url("https://169.254.169.254/secret-path-abc")
            .expect_err("must reject metadata address");
        assert!(!err.to_string().contains("169.254"));
        assert!(!err.to_string().contains("secret-path-abc"));
    }

    // --- Port value types + error mapping ---

    #[test]
    fn policy_error_maps_to_matching_egress_error() {
        assert!(matches!(
            EgressError::from(EgressPolicyError::NotHttps),
            EgressError::NotHttps
        ));
        assert!(matches!(
            EgressError::from(EgressPolicyError::BlockedAddress),
            EgressError::BlockedAddress
        ));
        assert!(matches!(
            EgressError::from(EgressPolicyError::MissingHost),
            EgressError::InvalidUrl
        ));
        assert!(matches!(
            EgressError::from(EgressPolicyError::MalformedUrl),
            EgressError::InvalidUrl
        ));
    }

    #[test]
    fn egress_request_carries_method_headers_and_body() {
        let req = EgressRequest {
            method: EgressMethod::Post,
            url: "https://example.com/hook".into(),
            headers: vec![("x-signature".into(), "abc".into())],
            body: Some(b"payload".to_vec()),
        };
        assert_eq!(req.method, EgressMethod::Post);
        assert_eq!(req.headers.len(), 1);
        assert_eq!(req.body.as_deref(), Some(b"payload".as_slice()));
    }

    #[test]
    fn debug_formatting_redacts_the_query_string_but_keeps_scheme_host_and_path() {
        // Regression guard: a QRZ login URL
        // carries the password as a query param. `{:?}` must never leak it,
        // even via a future stray debug-log call — only the base URL survives.
        let req = EgressRequest {
            method: EgressMethod::Get,
            url: "https://xmldata.qrz.com/xml/current/?username=W1AW&password=hunter2".into(),
            headers: Vec::new(),
            body: None,
        };
        let rendered = format!("{req:?}");
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("W1AW"));
        assert!(rendered.contains("https://xmldata.qrz.com/xml/current/?<redacted>"));
    }

    #[test]
    fn debug_formatting_leaves_a_query_less_url_untouched() {
        let req = EgressRequest {
            method: EgressMethod::Post,
            url: "https://example.com/hook".into(),
            headers: Vec::new(),
            body: None,
        };
        let rendered = format!("{req:?}");
        assert!(rendered.contains("https://example.com/hook"));
        assert!(!rendered.contains("<redacted>"));
    }

    proptest::proptest! {
        /// Invariant: any IPv4 address that falls inside a deny-table CIDR is
        /// always refused — the table can never be silently under-applied.
        #[test]
        fn any_address_in_a_denied_ipv4_range_is_blocked(raw in proptest::prelude::any::<u32>()) {
            let addr = Ipv4Addr::from(raw);
            let in_denied_range = BLOCKED_IPV4.iter().any(|(network, prefix)| {
                Ipv4Network::new_truncate(*network, *prefix)
                    .expect("deny-table IPv4 prefixes are valid")
                    .contains(addr)
            });
            if in_denied_range {
                proptest::prop_assert!(is_blocked_ip(IpAddr::V4(addr)));
            }
        }
    }
}
