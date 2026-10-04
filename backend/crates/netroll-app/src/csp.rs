// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The Content-Security-Policy the application emits on its own HTML.
//!
//! It lives here, not at a proxy, so it is versioned with the frontend that must
//! satisfy it and derived from the same config that shapes the page. Gated on
//! `Content-Type`, so there is no path list to keep in step with the router.

use std::collections::BTreeMap;
use std::fmt;

use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, HeaderName, HeaderValue, InvalidHeaderValue};
use axum::http::{Response, StatusCode};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::config::AppConfig;

/// Plausible's own event origin, used when a domain is configured but no
/// self-hosted script host is. MIRRORS `PLAUSIBLE_DEFAULT_HOST` in
/// `frontend/src/features/appConfig/appConfigApi.ts` — the tracker posts to
/// `<this>/api/event`, so the two literals must agree or `connect-src` refuses
/// the app's own analytics. The derived-requirements test welds them.
pub const PLAUSIBLE_DEFAULT_HOST: &str = "https://plausible.io";

/// Header name for an enforced policy.
pub const CONTENT_SECURITY_POLICY: HeaderName = HeaderName::from_static("content-security-policy");

/// Header name for a report-only policy: the browser logs violations and
/// enforces nothing — an operator's diagnostic during a rollout.
pub const CONTENT_SECURITY_POLICY_REPORT_ONLY: HeaderName =
    HeaderName::from_static("content-security-policy-report-only");

/// The resolved CSP header the app stamps on HTML: which NAME (enforce or
/// report-only) and the policy VALUE. Built once at boot, cloned into the layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CspHeader {
    name: HeaderName,
    value: HeaderValue,
}

impl CspHeader {
    /// Builds the header from a policy string. `report_only` selects the header
    /// name and nothing else — same value, same HTML-only placement. Fails only
    /// when the policy is not a valid header value, which neither boot path can
    /// hand in: `config::resolve_csp_policy` refuses an operator override with
    /// any byte outside visible ASCII, and [`default_policy`] is fixed literals
    /// plus origins that came through [`HostSource::parse`], whose authority
    /// allowlist is a strict subset of visible ASCII. The `Result` stays because
    /// the type cannot prove that about an arbitrary caller's string.
    pub fn new(policy: &str, report_only: bool) -> Result<Self, InvalidHeaderValue> {
        let name = if report_only {
            CONTENT_SECURITY_POLICY_REPORT_ONLY
        } else {
            CONTENT_SECURITY_POLICY
        };
        Ok(Self {
            name,
            value: HeaderValue::from_str(policy)?,
        })
    }

    /// The header name this policy is emitted under.
    pub fn name(&self) -> &HeaderName {
        &self.name
    }

    /// The policy value as it goes on the wire.
    pub fn value(&self) -> &HeaderValue {
        &self.value
    }
}

/// Derives the default policy from the configuration the page is served
/// under. Everything except `connect-src` is fixed — it describes the frontend
/// as built (`tests/csp_policy.rs` proves it against `frontend/dist`) — and
/// `connect-src` names `'self'`, the WebSocket origin of `public_base_url`
/// (`wss://` for `https://`, `ws://` for `http://`, host and port kept), and
/// the analytics origin when a Plausible domain is configured. Both configured
/// values reach the policy only as a [`HostSource`], so nothing that is not a
/// legal host-source is ever interpolated.
pub fn default_policy(public_base_url: &str, app_config: &AppConfig) -> String {
    let mut connect_src = vec!["'self'".to_owned()];
    // `config::resolve_public_base_url` refuses at boot, through this same
    // parser, everything `parse` returns `None` for — so from `main` the
    // `None` case is never taken. The function stays total for any other
    // caller rather than panicking, and names no origin it cannot derive.
    if let Some(origin) = HostSource::parse(public_base_url) {
        connect_src.push(origin.websocket_origin());
    }
    if let Some(origin) = analytics_origin(app_config) {
        connect_src.push(origin.to_string());
    }
    [
        "default-src 'self'".to_owned(),
        // No inline block in the built shell (theme-init.js is a file for
        // exactly this reason), so no hash and no nonce.
        "script-src 'self'".to_owned(),
        // The plain style-src is the fallback for browsers predating the CSP3
        // elem/attr split; it matches the pre-split behaviour rather than
        // tightening it.
        "style-src 'self' 'unsafe-inline'".to_owned(),
        // Injected <style> ELEMENTS are refused — the docs' mermaid shim depends
        // on this being enforced.
        "style-src-elem 'self'".to_owned(),
        // React emits style="" attributes throughout. Attributes cannot carry
        // selectors, so they cannot do the attribute-selector exfiltration a
        // <style> element can.
        "style-src-attr 'unsafe-inline'".to_owned(),
        // `https:` because parse_avatar_url accepts any HTTPS avatar host and
        // Gravatar is one; `data:` for Material for MkDocs' CSS-embedded SVG
        // icons under /docs, not for the SPA.
        "img-src 'self' data: https:".to_owned(),
        "font-src 'self'".to_owned(),
        // The WebSocket origin is spelled out because 'self' covering ws:/wss:
        // is inconsistent across browsers.
        format!("connect-src {}", connect_src.join(" ")),
        "frame-ancestors 'none'".to_owned(),
        // 'none', not 'self': the SPA sets no <base>, so refusing the element
        // outright costs nothing and closes base-tag hijacking entirely.
        "base-uri 'none'".to_owned(),
        "form-action 'self'".to_owned(),
        "object-src 'none'".to_owned(),
    ]
    .join("; ")
}

/// The origin the tracker posts events to, only when analytics is on at all.
/// `resolve_app_config` has already dropped a script host with no domain, so a
/// domain is the only switch that matters here. Reduced to scheme and
/// authority because under CSP path matching a sub-path host such as
/// `https://a.example/js` would NOT cover the tracker's POST to
/// `https://a.example/js/api/event`. A host `parse` cannot reduce contributes
/// nothing — `config::resolve_plausible_script_host` refuses it at boot, so
/// from `main` that never happens.
fn analytics_origin(app_config: &AppConfig) -> Option<HostSource> {
    app_config.plausible_domain.as_ref()?;
    let host = app_config
        .plausible_script_host
        .as_deref()
        .unwrap_or(PLAUSIBLE_DEFAULT_HOST);
    HostSource::parse(host)
}

/// The scheme of a [`HostSource`]: the only two an origin the app derives may
/// carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpScheme {
    /// Plain `http://` — legal for `PUBLIC_BASE_URL` on a loopback host only.
    Http,
    /// `https://`.
    Https,
}

impl HttpScheme {
    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    fn websocket(self) -> &'static str {
        match self {
            Self::Http => "ws",
            Self::Https => "wss",
        }
    }
}

/// An http(s) URL reduced to the one shape a CSP host-source may hold: the
/// scheme and a lower-cased `host[:port]`, with userinfo, path, query and
/// fragment dropped. Built only through [`HostSource::parse`], so holding one
/// is proof the authority carries nothing that could split or extend a
/// `;`-joined policy. `Display` renders it as `scheme://host[:port]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSource {
    scheme: HttpScheme,
    authority: String,
}

impl HostSource {
    /// Reduces `url` to a host-source, or `None` when it is not `http(s)://`,
    /// has no host, or its `host[:port]` holds a byte a host-source may not:
    /// only ASCII letters, digits, `-`, `.`, `:` and the IPv6 brackets pass.
    pub fn parse(url: &str) -> Option<Self> {
        let (scheme, rest) = url.trim().split_once("://")?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "https" => HttpScheme::Https,
            "http" => HttpScheme::Http,
            _ => return None,
        };
        let authority = rest.find(['/', '?', '#']).map_or(rest, |end| &rest[..end]);
        // Userinfo is not part of a host-source: a browser meeting one drops
        // the WHOLE source expression, not just the credentials.
        let host_port = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host_port)| host_port);
        if host_port.is_empty() || !host_port.bytes().all(is_host_source_byte) {
            return None;
        }
        Some(Self {
            scheme,
            authority: host_port.to_ascii_lowercase(),
        })
    }

    /// The scheme the URL carried.
    pub fn scheme(&self) -> HttpScheme {
        self.scheme
    }

    /// True for the three loopback spellings a developer serves the app on:
    /// `localhost`, `127.0.0.1` and `[::1]`, port ignored.
    pub fn is_loopback(&self) -> bool {
        matches!(self.host(), "localhost" | "127.0.0.1" | "[::1]")
    }

    /// `wss://host[:port]` for `https`, `ws://…` for `http`.
    pub fn websocket_origin(&self) -> String {
        format!("{}://{}", self.scheme.websocket(), self.authority)
    }

    fn host(&self) -> &str {
        // An IPv6 literal keeps its brackets and may be followed by a port.
        if self.authority.starts_with('[') {
            return self
                .authority
                .find(']')
                .map_or(&self.authority, |end| &self.authority[..=end]);
        }
        self.authority
            .split_once(':')
            .map_or(&self.authority, |(host, _)| host)
    }
}

impl fmt::Display for HostSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme.as_str(), self.authority)
    }
}

fn is_host_source_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b':' | b'[' | b']')
}

/// A `tower` layer that stamps `header` onto every HTML response and every
/// `304 Not Modified`, and leaves every other response untouched.
///
/// The 304 is stamped because `ServeDir` answers a revalidation with an empty
/// body and NO `Content-Type`, so a content-type gate alone would let a
/// returning browser keep enforcing the policy it cached the page under — a
/// config-only restart never touches index.html's mtime, so a new policy would
/// never reach it. A 304 for a stylesheet now carries the header too, which a
/// browser ignores on a non-document response; that is what makes gating on
/// the status safe.
///
/// `overriding` rather than `if_not_present`: nothing upstream in this process
/// sets a CSP today, and if something ever did, the configured policy must win
/// rather than be silently shadowed.
pub fn html_only_layer(
    header: CspHeader,
) -> SetResponseHeaderLayer<impl Fn(&Response<Body>) -> Option<HeaderValue> + Clone> {
    let CspHeader { name, value } = header;
    SetResponseHeaderLayer::overriding(name, move |response: &Response<Body>| {
        (is_html(response) || response.status() == StatusCode::NOT_MODIFIED).then(|| value.clone())
    })
}

/// True when the response's media type is HTML — `text/html` or
/// `application/xhtml+xml`, parameters and case ignored. `ServeDir`/`ServeFile`
/// set `Content-Type` from the extension, so this is what tells the shell and
/// the docs apart from the JS, CSS and images served beside them. The media
/// type is compared whole, never prefix-matched, and deliberately NOT `text/*`:
/// plain text and CSS are text.
fn is_html(response: &Response<Body>) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| {
            let media_type = content_type
                .find(';')
                .map_or(content_type, |end| &content_type[..end])
                .trim();
            media_type.eq_ignore_ascii_case("text/html")
                || media_type.eq_ignore_ascii_case("application/xhtml+xml")
        })
}

/// The directive names CSP Levels 1–3 define, plus the deprecated ones
/// browsers still recognise. A clause naming anything else is discarded.
const DIRECTIVE_NAMES: &[&str] = &[
    "default-src",
    "script-src",
    "script-src-elem",
    "script-src-attr",
    "style-src",
    "style-src-elem",
    "style-src-attr",
    "img-src",
    "font-src",
    "connect-src",
    "media-src",
    "object-src",
    "child-src",
    "frame-src",
    "worker-src",
    "manifest-src",
    "prefetch-src",
    "fenced-frame-src",
    "base-uri",
    "sandbox",
    "form-action",
    "frame-ancestors",
    "navigate-to",
    "report-uri",
    "report-to",
    "upgrade-insecure-requests",
    "block-all-mixed-content",
    "require-trusted-types-for",
    "trusted-types",
    "plugin-types",
    "referrer",
];

/// True when `name` is a CSP directive a browser recognises, case-insensitive.
/// A policy in which no clause names one of these is, to a browser, no policy
/// at all — `config::resolve_csp_policy` refuses such an override.
pub fn is_directive_name(name: &str) -> bool {
    DIRECTIVE_NAMES
        .iter()
        .any(|directive| directive.eq_ignore_ascii_case(name))
}

/// A policy parsed into its directives, for asserting on structure rather than
/// on the header string. Lookups through [`Policy::sources`] apply the same
/// fallback a browser does, so a test asks the question the browser asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    directives: BTreeMap<String, Vec<String>>,
}

impl Policy {
    /// Parses a serialized policy: directives split on `;`, each a name followed
    /// by whitespace-separated source expressions. Names are lower-cased; the
    /// first occurrence of a directive wins, as in CSP.
    pub fn parse(policy: &str) -> Self {
        let mut directives = BTreeMap::new();
        for clause in policy.split(';') {
            let mut tokens = clause.split_whitespace();
            let Some(name) = tokens.next() else {
                continue;
            };
            let sources: Vec<String> = tokens.map(str::to_owned).collect();
            directives
                .entry(name.to_ascii_lowercase())
                .or_insert(sources);
        }
        Self { directives }
    }

    /// The lower-cased name of every directive the policy carries, in
    /// lexical order.
    pub fn directive_names(&self) -> impl Iterator<Item = &str> {
        self.directives.keys().map(String::as_str)
    }

    /// The source list of exactly `name`, with no fallback. `None` when the
    /// policy does not name that directive.
    pub fn directive(&self, name: &str) -> Option<&[String]> {
        self.directives.get(name).map(Vec::as_slice)
    }

    /// The source list that GOVERNS `name`, following the CSP fallback chain:
    /// `*-src-elem`/`*-src-attr` fall back to their `*-src`, fetch directives
    /// fall back to `default-src`, and the document directives
    /// (`frame-ancestors`, `base-uri`, `form-action`, `sandbox`, the report
    /// directives) never fall back. `None` when nothing governs it.
    pub fn sources(&self, name: &str) -> Option<&[String]> {
        let mut candidate = Some(name.to_ascii_lowercase());
        while let Some(current) = candidate {
            if let Some(sources) = self.directives.get(&current) {
                return Some(sources);
            }
            candidate = fallback_of(&current).map(str::to_owned);
        }
        None
    }
}

/// The directive a browser consults next when `directive` is absent.
fn fallback_of(directive: &str) -> Option<&'static str> {
    match directive {
        "script-src-elem" | "script-src-attr" => Some("script-src"),
        "style-src-elem" | "style-src-attr" => Some("style-src"),
        "default-src"
        | "frame-ancestors"
        | "base-uri"
        | "form-action"
        | "sandbox"
        | "report-uri"
        | "report-to"
        | "upgrade-insecure-requests" => None,
        _ => Some("default-src"),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, Request, StatusCode};
    use tower::{Layer, ServiceExt};

    use super::*;

    fn analytics(domain: Option<&str>, host: Option<&str>) -> AppConfig {
        crate::config::resolve_app_config(domain.map(str::to_owned), host.map(str::to_owned), None)
    }

    fn connect_src(policy: &str) -> Vec<String> {
        Policy::parse(policy)
            .directive("connect-src")
            .expect("the default names connect-src")
            .to_vec()
    }

    /// Source expressions are matched case-insensitively by a browser, and
    /// `Policy::parse` lower-cases directive NAMES only — so a negative check
    /// on `'unsafe-inline'` must not be fooled by `'UNSAFE-INLINE'`.
    fn has_source(sources: &[String], token: &str) -> bool {
        sources.iter().any(|s| s.eq_ignore_ascii_case(token))
    }

    #[test]
    fn a_configured_value_that_is_not_a_host_source_contributes_nothing_to_the_policy() {
        // The policy is `;`-joined and the FIRST occurrence of a directive wins
        // in a browser, so a value carrying `;` would land its own
        // frame-ancestors ahead of the real one. Boot refuses these; the
        // derivation itself must ALSO never let one through.
        let off = AppConfig::default();
        for injected in [
            "https://a.example; frame-ancestors *",
            "https://a.example https://b.example",
            "https://a.example,https://b.example",
        ] {
            let via_analytics = Policy::parse(&default_policy(
                "https://netroll.example",
                &analytics(Some("netroll.example"), Some(injected)),
            ));
            assert_eq!(
                via_analytics.directive("frame-ancestors"),
                Some(&["'none'".to_owned()][..]),
                "analytics host {injected:?}"
            );
            assert_eq!(
                via_analytics.directive("connect-src"),
                Some(&["'self'".to_owned(), "wss://netroll.example".to_owned()][..]),
                "analytics host {injected:?}"
            );

            let via_base_url = Policy::parse(&default_policy(injected, &off));
            assert_eq!(
                via_base_url.directive("frame-ancestors"),
                Some(&["'none'".to_owned()][..]),
                "base url {injected:?}"
            );
            assert_eq!(
                via_base_url.directive("connect-src"),
                Some(&["'self'".to_owned()][..]),
                "base url {injected:?}"
            );
        }
    }

    #[test]
    fn websocket_origin_drops_userinfo_and_lower_cases_the_authority() {
        // Userinfo is not legal in a host-source — a browser drops the whole
        // source expression, not just the credentials — and host matching is
        // case-insensitive, so the canonical form is lower-case.
        let sources = connect_src(&default_policy(
            "https://user:pw@Netroll.Example:8443/app",
            &AppConfig::default(),
        ));
        assert_eq!(
            sources,
            vec!["'self'".to_owned(), "wss://netroll.example:8443".to_owned()]
        );
    }

    #[test]
    fn analytics_origin_is_reduced_to_scheme_and_authority() {
        // Under CSP path matching `https://a.example/js` does NOT cover the
        // tracker's POST to `https://a.example/js/api/event`; the origin does.
        // The configured value keeps its path for the frontend — the policy
        // names the origin.
        let sources = connect_src(&default_policy(
            "https://netroll.example",
            &analytics(
                Some("netroll.example"),
                Some("https://Analytics.Example/js/"),
            ),
        ));
        assert!(
            sources.contains(&"https://analytics.example".to_owned()),
            "{sources:?}"
        );
        assert!(!sources.iter().any(|s| s.contains("/js")), "{sources:?}");
    }

    #[test]
    fn default_policy_derives_the_websocket_origin_from_public_base_url() {
        let off = AppConfig::default();

        assert!(
            connect_src(&default_policy("https://netroll.example", &off))
                .contains(&"wss://netroll.example".to_owned()),
            "https ⇒ wss on the same host"
        );
        let dev = connect_src(&default_policy("http://localhost:5173", &off));
        assert!(
            dev.contains(&"ws://localhost:5173".to_owned()),
            "http ⇒ ws, port preserved, got {dev:?}"
        );
        assert!(
            !dev.iter().any(|s| s.starts_with("wss://")),
            "a plain-http origin must not claim a wss: origin"
        );
        // Only the origin: a path or trailing slash on PUBLIC_BASE_URL must not
        // leak into the source expression.
        assert!(
            connect_src(&default_policy("https://netroll.example/app/", &off))
                .contains(&"wss://netroll.example".to_owned())
        );
    }

    #[test]
    fn a_base_url_boot_would_refuse_yields_self_alone_rather_than_an_invented_origin() {
        // `resolve_public_base_url` refuses a non-http(s) value, so from `main`
        // this cannot happen; the derivation stays total for any other caller
        // and must never invent an origin it cannot derive.
        let sources = connect_src(&default_policy(
            "ftp://netroll.example",
            &AppConfig::default(),
        ));
        assert_eq!(sources, vec!["'self'".to_owned()]);
    }

    #[test]
    fn default_policy_names_the_plausible_origin_only_when_a_domain_is_configured() {
        let base = "https://netroll.example";

        // Domain + self-hosted host ⇒ that host, trailing slash dropped.
        let hosted = connect_src(&default_policy(
            base,
            &analytics(Some("netroll.example"), Some("https://analytics.example/")),
        ));
        assert!(
            hosted.contains(&"https://analytics.example".to_owned()),
            "{hosted:?}"
        );
        assert!(!hosted.contains(&PLAUSIBLE_DEFAULT_HOST.to_owned()));

        // Domain, no host ⇒ Plausible's own origin, the literal the tracker uses.
        let cloud = connect_src(&default_policy(
            base,
            &analytics(Some("netroll.example"), None),
        ));
        assert!(
            cloud.contains(&PLAUSIBLE_DEFAULT_HOST.to_owned()),
            "{cloud:?}"
        );

        // No domain ⇒ analytics is off and NO https origin is opened, even if a
        // host was set (resolve_app_config already drops it).
        let off = connect_src(&default_policy(
            base,
            &analytics(None, Some("https://analytics.example")),
        ));
        assert_eq!(
            off,
            vec!["'self'".to_owned(), "wss://netroll.example".to_owned()]
        );
    }

    #[test]
    fn default_policy_has_the_full_deployed_shape_when_both_origins_are_configured() {
        // Every directive a deployed policy carries, derived from example
        // configuration. Compared as PARSED directives, not as a string: order
        // and whitespace are not the contract, the source lists are. Checking
        // the derivation against a LIVE host's header is an operator runbook
        // step, not a literal in the repo.
        let expected = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                        style-src-elem 'self'; style-src-attr 'unsafe-inline'; img-src 'self' data: https:; \
                        font-src 'self'; connect-src 'self' wss://netroll.example https://analytics.example; \
                        frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'";
        let derived = default_policy(
            "https://netroll.example",
            &analytics(Some("netroll.example"), Some("https://analytics.example")),
        );

        assert_eq!(Policy::parse(&derived), Policy::parse(expected));
    }

    #[test]
    fn parse_splits_directives_and_source_lists() {
        let policy = Policy::parse(
            "default-src 'self';  Script-Src 'self' https://a.example ;style-src 'self' 'unsafe-inline'; \
             style-src-elem 'self';frame-ancestors 'none'; object-src 'none'; script-src 'unsafe-inline'",
        );

        assert_eq!(
            policy.directive("script-src"),
            Some(&["'self'".to_owned(), "https://a.example".to_owned()][..]),
            "names are lower-cased and the FIRST occurrence wins"
        );
        assert_eq!(
            policy.directive("img-src"),
            None,
            "exact lookup has no fallback"
        );
        // Fetch directives fall back to default-src.
        assert_eq!(policy.sources("img-src"), Some(&["'self'".to_owned()][..]));
        assert_eq!(
            policy.sources("connect-src"),
            Some(&["'self'".to_owned()][..])
        );
        // *-elem / *-attr fall back to their *-src, then default-src.
        assert_eq!(
            policy.sources("style-src-elem"),
            Some(&["'self'".to_owned()][..])
        );
        assert_eq!(
            policy.sources("style-src-attr"),
            Some(&["'self'".to_owned(), "'unsafe-inline'".to_owned()][..])
        );
        assert_eq!(
            policy.sources("script-src-elem"),
            Some(&["'self'".to_owned(), "https://a.example".to_owned()][..])
        );
        // Document directives never fall back.
        assert_eq!(
            policy.sources("frame-ancestors"),
            Some(&["'none'".to_owned()][..])
        );
        assert_eq!(policy.sources("base-uri"), None);
        assert_eq!(policy.sources("form-action"), None);
    }

    #[test]
    fn structural_invariants_hold_on_the_default() {
        let policy = Policy::parse(&default_policy(
            "https://netroll.example",
            &analytics(Some("netroll.example"), Some("https://analytics.example")),
        ));
        let script = policy.sources("script-src").expect("script-src governed");
        assert!(!has_source(script, "'unsafe-inline'"));
        assert!(has_source(script, "'self'"));
        for (name, sources) in &policy.directives {
            assert!(
                !has_source(sources, "'unsafe-eval'"),
                "'unsafe-eval' in {name}"
            );
        }
        let default = policy
            .directive("default-src")
            .expect("default-src present");
        for forbidden in ["*", "http:", "https:"] {
            assert!(
                !has_source(default, forbidden),
                "{forbidden} in default-src"
            );
        }
        assert_eq!(
            policy.directive("object-src"),
            Some(&["'none'".to_owned()][..])
        );
        // The SPA sets no <base>, so the element is refused outright.
        assert_eq!(
            policy.directive("base-uri"),
            Some(&["'none'".to_owned()][..])
        );
        assert_eq!(
            policy.directive("frame-ancestors"),
            Some(&["'none'".to_owned()][..])
        );
        assert_eq!(
            policy.directive("form-action"),
            Some(&["'self'".to_owned()][..])
        );
        // React emits style="" attributes throughout; the CSP3 elem/attr split
        // is what lets those work while injected <style> ELEMENTS stay refused.
        assert_eq!(
            policy.directive("style-src-attr"),
            Some(&["'unsafe-inline'".to_owned()][..])
        );
        assert_eq!(
            policy.directive("style-src-elem"),
            Some(&["'self'".to_owned()][..])
        );
    }

    async fn stamped_with_status(status: StatusCode, content_type: Option<&str>) -> HeaderMap {
        let header = CspHeader::new("default-src 'self'", false).expect("valid policy");
        let content_type = content_type.map(str::to_owned);
        let service =
            html_only_layer(header).layer(tower::service_fn(move |_request: Request<Body>| {
                let content_type = content_type.clone();
                async move {
                    let mut response = Response::builder().status(status);
                    if let Some(content_type) = content_type {
                        response = response.header(CONTENT_TYPE, content_type);
                    }
                    Ok::<_, std::convert::Infallible>(response.body(Body::empty()).unwrap())
                }
            }));
        let response = service
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        response.headers().clone()
    }

    async fn stamped(content_type: Option<&str>) -> HeaderMap {
        stamped_with_status(StatusCode::OK, content_type).await
    }

    #[tokio::test]
    async fn layer_stamps_only_responses_whose_media_type_is_html() {
        for html in [
            "text/html",
            "text/html; charset=utf-8",
            "text/html ; charset=utf-8",
            "TEXT/HTML",
            // XHTML is HTML for CSP purposes.
            "application/xhtml+xml",
            "Application/XHTML+XML; charset=utf-8",
        ] {
            let headers = stamped(Some(html)).await;
            assert_eq!(
                headers.get_all(CONTENT_SECURITY_POLICY).iter().count(),
                1,
                "exactly one CSP on {html:?}"
            );
            assert_eq!(
                headers.get(CONTENT_SECURITY_POLICY).unwrap(),
                "default-src 'self'"
            );
        }
        // Not `text/*`: plain text and CSS are text and must NOT be stamped.
        // And the media type is COMPARED, not prefix-matched: `text/htmlx` is
        // not HTML.
        for other in [
            Some("application/json"),
            Some("application/problem+json"),
            Some("text/plain"),
            Some("text/css"),
            Some("text/javascript"),
            Some("text/htmlx"),
            Some("image/png"),
            None,
        ] {
            let headers = stamped(other).await;
            assert!(
                headers.get(CONTENT_SECURITY_POLICY).is_none(),
                "no CSP on {other:?}"
            );
        }
    }

    #[tokio::test]
    async fn layer_stamps_a_304_revalidation_that_carries_no_content_type() {
        // ServeDir answers a conditional GET with an empty 304 and no
        // Content-Type, so a content-type gate alone would leave a returning
        // browser enforcing whatever policy it cached the page under — a
        // config-only restart (report-only, a new analytics host) would appear
        // not to take effect until the file's mtime changed.
        let headers = stamped_with_status(StatusCode::NOT_MODIFIED, None).await;
        assert_eq!(
            headers.get(CONTENT_SECURITY_POLICY).unwrap(),
            "default-src 'self'"
        );
    }

    #[test]
    fn header_name_follows_the_report_only_switch_and_the_value_does_not() {
        let enforce = CspHeader::new("default-src 'self'", false).unwrap();
        let report = CspHeader::new("default-src 'self'", true).unwrap();
        assert_eq!(enforce.name(), &CONTENT_SECURITY_POLICY);
        assert_eq!(report.name(), &CONTENT_SECURITY_POLICY_REPORT_ONLY);
        assert_eq!(enforce.value(), report.value());
    }
}
