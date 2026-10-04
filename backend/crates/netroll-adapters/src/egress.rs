// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The I/O half of the SSRF defence; the pure classifier lives in
//! `netroll_domain::egress`. The load-bearing layer is RESOLVE-THEN-PIN: the
//! resolver returns only validated IPs, so reqwest connects to exactly the
//! address checked and the rebinding window is closed, on every redirect hop.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use netroll_domain::egress::{
    Egress, EgressError, EgressMethod, EgressRequest, EgressResponse, is_blocked_ip,
    validate_egress_url,
};
use netroll_domain::ports::BoxFuture;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;
use reqwest::{Client, Method};
use url::{Host, Url};

/// Cap on establishing the TCP+TLS connection — an unresponsive or tarpit host
/// must not pin the calling task while the socket hangs.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Cap on the entire request/response exchange, including the streamed body.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum redirect hops followed before refusing — each hop is re-validated,
/// but the chain length is also bounded to stop redirect loops/amplification.
const MAX_REDIRECTS: usize = 3;
/// Maximum response body accepted (1 MiB) — a webhook/lookup response is small;
/// enforced while streaming so a lying/omitted `Content-Length` cannot smuggle
/// an unbounded body.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Boxed error type the reqwest DNS resolver and redirect policy return.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Injectable name→addresses lookup. Production uses the system resolver; tests
/// supply a deterministic fake so no real DNS or network is touched.
type LookupFn =
    Arc<dyn Fn(String) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>> + Send + Sync>;

/// A refusal raised by the resolver or the redirect policy, carried through
/// reqwest's error chain so [`map_reqwest_error`] can recover the exact cause.
#[derive(Debug)]
enum EgressGuardError {
    Blocked,
    NotHttps,
    TooManyRedirects,
}

impl std::fmt::Display for EgressGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Fixed text only — never echo the target.
        let msg = match self {
            EgressGuardError::Blocked => "egress target is a blocked address",
            EgressGuardError::NotHttps => "egress redirect is not https",
            EgressGuardError::TooManyRedirects => "egress exceeded the redirect limit",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for EgressGuardError {}

/// The decision the redirect policy makes for one hop.
#[derive(Debug, PartialEq, Eq)]
enum RedirectDecision {
    Follow,
    Blocked,
    NotHttps,
    TooManyRedirects,
}

/// Custom DNS resolver that validates every resolved address and returns only
/// validated ones — the resolve-then-pin control (see module docs).
struct ValidatingResolver {
    lookup: LookupFn,
}

impl Resolve for ValidatingResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let lookup = self.lookup.clone();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let ips = lookup(host).await.map_err(|e| Box::new(e) as BoxError)?;
            // Fail closed: if ANY resolved address is blocked, refuse the whole
            // name — a name resolving to both a public and a private address is
            // hostile, and we must not silently connect to the safe sibling.
            if ips.iter().copied().any(is_blocked_ip) {
                return Err(Box::new(EgressGuardError::Blocked) as BoxError);
            }
            // Return only validated addresses with port 0 (reqwest sets the real
            // port) — reqwest connects to exactly these, so this is the pin.
            let addrs: Vec<SocketAddr> = ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect();
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Redirect decision for one hop: bound the count, require https, and refuse a
/// denied IP-literal `Location`. Hostname `Location`s are re-validated by the
/// resolver on the next connection, so only literals need checking here.
fn evaluate_redirect(previous_hops: usize, next: &Url) -> RedirectDecision {
    // `previous_hops` counts the URLs already requested in this chain, so it is
    // the number of redirects already followed; refuse once it passes the bound.
    if previous_hops > MAX_REDIRECTS {
        return RedirectDecision::TooManyRedirects;
    }
    if next.scheme() != "https" {
        return RedirectDecision::NotHttps;
    }
    match next.host() {
        Some(Host::Ipv4(addr)) if is_blocked_ip(IpAddr::V4(addr)) => RedirectDecision::Blocked,
        Some(Host::Ipv6(addr)) if is_blocked_ip(IpAddr::V6(addr)) => RedirectDecision::Blocked,
        // `url` rejects an empty host for `https` before this ever runs, but
        // guard it explicitly anyway — the domain crate's `validate_egress_url`
        // treats an empty host as a refusal, so the redirect path must not be
        // the one place that silently lets it through.
        None | Some(Host::Domain("")) => RedirectDecision::Blocked,
        _ => RedirectDecision::Follow,
    }
}

/// Appends `chunk` to `buf`, refusing once the running total exceeds `cap`.
/// Enforced during the streamed read so `Content-Length` is never trusted.
fn accumulate_capped(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> Result<(), EgressError> {
    if buf.len() + chunk.len() > cap {
        return Err(EgressError::ResponseTooLarge);
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// Recovers the domain [`EgressError`] from a reqwest failure, preferring our
/// sentinel guard errors carried in the source chain, then timeout/redirect
/// classification, and finally a bounded transport message (no URL leak).
fn map_reqwest_error(err: reqwest::Error) -> EgressError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&err);
    while let Some(e) = source {
        if let Some(guard) = e.downcast_ref::<EgressGuardError>() {
            return match guard {
                EgressGuardError::Blocked => EgressError::BlockedAddress,
                EgressGuardError::NotHttps => EgressError::NotHttps,
                EgressGuardError::TooManyRedirects => EgressError::TooManyRedirects,
            };
        }
        source = e.source();
    }
    if err.is_timeout() {
        return EgressError::Timeout;
    }
    if err.is_redirect() {
        return EgressError::TooManyRedirects;
    }
    EgressError::Transport(bounded_transport_message(&err))
}

/// A fixed, category-level transport message that never echoes the target URL.
fn bounded_transport_message(err: &reqwest::Error) -> String {
    if err.is_connect() {
        "connection failed".to_owned()
    } else if err.is_request() {
        "request failed".to_owned()
    } else {
        "transport failure".to_owned()
    }
}

/// The production system resolver: a real DNS lookup via tokio.
fn system_lookup() -> LookupFn {
    Arc::new(|host: String| {
        Box::pin(async move {
            // Port 0 is a placeholder — reqwest overrides it with the URL's port.
            let addrs = tokio::net::lookup_host((host.as_str(), 0u16)).await?;
            Ok(addrs.map(|sa| sa.ip()).collect())
        })
    })
}

/// Builds the locked-down reqwest client around `lookup`.
fn build_client(lookup: LookupFn) -> Result<Client, EgressError> {
    Client::builder()
        .dns_resolver(Arc::new(ValidatingResolver { lookup }))
        .redirect(redirect_policy())
        .https_only(true)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .build()
        .map_err(map_reqwest_error)
}

/// The bounded, per-hop-re-validating redirect policy.
fn redirect_policy() -> redirect::Policy {
    redirect::Policy::custom(|attempt| {
        match evaluate_redirect(attempt.previous().len(), attempt.url()) {
            RedirectDecision::Follow => attempt.follow(),
            RedirectDecision::Blocked => attempt.error(EgressGuardError::Blocked),
            RedirectDecision::NotHttps => attempt.error(EgressGuardError::NotHttps),
            RedirectDecision::TooManyRedirects => attempt.error(EgressGuardError::TooManyRedirects),
        }
    })
}

/// SSRF-safe outbound HTTP client. Cheap to clone (shares a pooled
/// `reqwest::Client`), so construct once and share rather than rebuilding.
#[derive(Clone)]
pub struct SsrfSafeEgress {
    client: Client,
}

impl SsrfSafeEgress {
    /// Builds the locked-down client using the real system DNS resolver.
    pub fn new() -> Result<Self, EgressError> {
        Ok(Self {
            client: build_client(system_lookup())?,
        })
    }

    /// Builds the client with an injected name→addresses lookup — the test seam
    /// for deterministic, network-free behavioral tests.
    #[cfg(test)]
    fn with_lookup(lookup: LookupFn) -> Result<Self, EgressError> {
        Ok(Self {
            client: build_client(lookup)?,
        })
    }
}

impl Egress for SsrfSafeEgress {
    fn send<'a>(
        &'a self,
        req: EgressRequest,
    ) -> BoxFuture<'a, Result<EgressResponse, EgressError>> {
        Box::pin(async move {
            // Up-front policy gate: refuse a non-https scheme or a denied
            // IP-literal host BEFORE any socket is opened. reqwest may connect
            // to an IP literal without consulting the resolver, so this check —
            // not the resolver — is what closes the IP-literal hole.
            validate_egress_url(&req.url)?;

            let method = match req.method {
                EgressMethod::Get => Method::GET,
                EgressMethod::Post => Method::POST,
            };
            let mut builder = self.client.request(method, &req.url);
            for (name, value) in &req.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            if let Some(body) = req.body {
                builder = builder.body(body);
            }

            let mut response = builder.send().await.map_err(map_reqwest_error)?;
            let status = response.status().as_u16();

            let mut buf = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(map_reqwest_error)? {
                accumulate_capped(&mut buf, &chunk, MAX_RESPONSE_BYTES)?;
            }

            Ok(EgressResponse { status, body: buf })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("test address parses")
    }

    fn get(url: &str) -> EgressRequest {
        EgressRequest {
            method: EgressMethod::Get,
            url: url.to_owned(),
            headers: Vec::new(),
            body: None,
        }
    }

    fn post(url: &str) -> EgressRequest {
        EgressRequest {
            method: EgressMethod::Post,
            url: url.to_owned(),
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: Some(b"{\"embeds\":[]}".to_vec()),
        }
    }

    /// A lookup that fails the test if it is ever called (proves "no I/O").
    fn never_lookup(flag: Arc<AtomicBool>) -> LookupFn {
        Arc::new(move |_host| {
            flag.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(Vec::new()) })
        })
    }

    fn fixed_lookup(addrs: Vec<IpAddr>) -> LookupFn {
        Arc::new(move |_host| {
            let addrs = addrs.clone();
            Box::pin(async move { Ok(addrs) })
        })
    }

    // --- DNS-rebinding — the resolver re-validates on every lookup ---

    #[tokio::test]
    async fn resolver_refuses_a_rebind_to_a_denied_address_on_a_later_lookup() {
        // Safe on the first lookup (config time), denied on the second (delivery).
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let lookup: LookupFn = Arc::new(move |_host| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if n == 0 {
                    Ok(vec![ip("93.184.216.34")])
                } else {
                    Ok(vec![ip("169.254.169.254")])
                }
            })
        });
        let resolver = ValidatingResolver { lookup };

        let first = resolver
            .resolve(Name::from_str("rebind.example").unwrap())
            .await;
        assert!(first.is_ok(), "the safe first resolution is allowed");

        let second = resolver
            .resolve(Name::from_str("rebind.example").unwrap())
            .await;
        let err = second.err().expect("the rebind must be refused");
        assert!(
            err.downcast_ref::<EgressGuardError>()
                .is_some_and(|g| matches!(g, EgressGuardError::Blocked)),
            "the delivery-time re-validation binds to the actually-resolved address"
        );
    }

    #[tokio::test]
    async fn send_refuses_a_hostname_that_resolves_to_a_denied_address() {
        let egress =
            SsrfSafeEgress::with_lookup(fixed_lookup(vec![ip("169.254.169.254")])).unwrap();
        let err = egress
            .send(get("https://rebind.example/"))
            .await
            .unwrap_err();
        assert!(matches!(err, EgressError::BlockedAddress));
    }

    #[tokio::test]
    async fn resolver_fails_closed_when_one_of_several_addresses_is_denied() {
        // A name resolving to both a public and a private address is hostile.
        let egress =
            SsrfSafeEgress::with_lookup(fixed_lookup(vec![ip("93.184.216.34"), ip("127.0.0.1")]))
                .unwrap();
        let err = egress
            .send(get("https://mixed.example/"))
            .await
            .unwrap_err();
        assert!(matches!(err, EgressError::BlockedAddress));
    }

    // --- `discord.com` gets NO exemption ---
    //
    // The point of this test is what it would take to make it pass wrongly.
    // There is no allowlist in this crate or in `netroll_domain::egress` to
    // remove an entry from — it is a DENY design (two CIDR deny tables, an
    // `!is_global` backstop, four IPv6-embedded-IPv4 unwrappings) — so "no
    // exemption" cannot be proved by deleting one. It is proved by showing that
    // a `discord.com` URL shaped exactly like a real Discord webhook, whose name
    // resolves into a denied range, is REFUSED by the same machinery every other
    // host goes through.
    //
    // It also cannot be an integration test: `with_lookup` is `#[cfg(test)]` AND
    // private, so the only place a deterministic hostile resolution can be
    // injected is right here. The delivery-side test in
    // `netroll-app/tests/api_on_close_delivery.rs` proves the POST funnels
    // through the `Egress` PORT, and proves nothing about SSRF — the fake it
    // captures IS the substitute for this client.
    //
    // The mutation that must red this: create the exemption the policy forbids. Note
    // WHERE it has to go — an early return in `validate_egress_url` for
    // `host == "discord.com"` changes nothing, because `Host::Domain(_)` already
    // passes that function unconditionally; the guard that actually protects a
    // HOSTNAME is `ValidatingResolver::resolve` above.

    #[tokio::test]
    async fn send_refuses_a_discord_webhook_url_whose_host_resolves_into_a_denied_range() {
        for denied in ["127.0.0.1", "169.254.169.254", "10.0.0.7", "::1"] {
            let egress = SsrfSafeEgress::with_lookup(fixed_lookup(vec![ip(denied)])).unwrap();
            let err = egress
                .send(post("https://discord.com/api/webhooks/1234567890/tok-abc"))
                .await
                .unwrap_err();
            assert!(
                matches!(err, EgressError::BlockedAddress),
                "discord.com resolving to {denied} must be refused like any other host, \
                 got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_discord_webhook_url_resolving_publicly_is_not_refused_by_policy() {
        // The control for the test above: the refusal must come from the
        // ADDRESS, not from the host name or the URL shape. A public resolution
        // gets past every policy layer and fails only on the connection, which
        // is a transport error rather than a refusal — so a mutation that
        // blanket-blocked `discord.com` would not be hidden by this pair.
        // The same globally-routable address the rebind test above uses as its
        // "safe" resolution. NOT a documentation range like 192.0.2.0/24 —
        // `Ipv4Addr::is_global` excludes those, so the `!is_global` backstop
        // would refuse it and this control would assert nothing.
        let egress = SsrfSafeEgress::with_lookup(fixed_lookup(vec![ip("93.184.216.34")])).unwrap();
        let err = egress
            .send(post("https://discord.com/api/webhooks/1234567890/tok-abc"))
            .await
            .unwrap_err();
        assert!(
            !matches!(err, EgressError::BlockedAddress | EgressError::NotHttps),
            "a publicly-resolving discord.com URL is not refused by policy, got {err:?}"
        );
    }

    // --- HTTPS-only, refused before any I/O ---

    #[tokio::test]
    async fn send_refuses_non_https_scheme_before_any_io() {
        let called = Arc::new(AtomicBool::new(false));
        let egress = SsrfSafeEgress::with_lookup(never_lookup(called.clone())).unwrap();
        let err = egress.send(get("http://example.com/")).await.unwrap_err();
        assert!(matches!(err, EgressError::NotHttps));
        assert!(
            !called.load(Ordering::SeqCst),
            "no resolution/socket may happen for a rejected scheme"
        );
    }

    // --- Bare denied IP-literal URL is refused up front ---

    #[tokio::test]
    async fn send_refuses_a_bare_denied_ip_literal_url() {
        let called = Arc::new(AtomicBool::new(false));
        let egress = SsrfSafeEgress::with_lookup(never_lookup(called.clone())).unwrap();
        let err = egress
            .send(get("https://169.254.169.254/latest/meta-data/"))
            .await
            .unwrap_err();
        assert!(matches!(err, EgressError::BlockedAddress));
        assert!(
            !called.load(Ordering::SeqCst),
            "an IP-literal host is caught up front, before any socket"
        );
    }

    // --- Redirect bound + per-hop re-validation (decision function) ---

    #[test]
    fn redirect_beyond_the_limit_is_refused() {
        let url = Url::parse("https://example.com/next").unwrap();
        assert_eq!(
            evaluate_redirect(MAX_REDIRECTS + 1, &url),
            RedirectDecision::TooManyRedirects
        );
        assert_eq!(
            evaluate_redirect(MAX_REDIRECTS, &url),
            RedirectDecision::Follow
        );
    }

    #[test]
    fn redirect_to_non_https_is_refused() {
        let url = Url::parse("http://example.com/next").unwrap();
        assert_eq!(evaluate_redirect(0, &url), RedirectDecision::NotHttps);
    }

    #[test]
    fn redirect_to_denied_ip_literal_is_refused() {
        let url = Url::parse("https://169.254.169.254/").unwrap();
        assert_eq!(evaluate_redirect(0, &url), RedirectDecision::Blocked);
    }

    #[test]
    fn redirect_to_global_https_is_followed() {
        let url = Url::parse("https://example.com/next").unwrap();
        assert_eq!(evaluate_redirect(1, &url), RedirectDecision::Follow);
    }

    // --- Streamed response-size cap ---

    #[test]
    fn body_within_the_cap_accumulates() {
        let mut buf = Vec::new();
        assert!(accumulate_capped(&mut buf, b"hello", 10).is_ok());
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn body_exceeding_the_cap_is_refused_mid_stream() {
        let mut buf = Vec::new();
        accumulate_capped(&mut buf, &[0u8; 8], 10).unwrap();
        let err = accumulate_capped(&mut buf, &[0u8; 8], 10).unwrap_err();
        assert!(matches!(err, EgressError::ResponseTooLarge));
    }
}
