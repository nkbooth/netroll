// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The Content-Security-Policy the app emits, checked against the frontend as
//! built: `requirements()` derives what `dist/index.html` needs and
//! `evaluate()` judges the policy as a browser would. `font-src`/`img-src` are
//! asserted rather than derived, and `/docs` runs on a fixture. FAILS rather
//! than skips when `dist/` is absent or stale — a green on no build proves nothing.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use base64::Engine;
use netroll_app::config::{AppConfig, ConfigError, resolve_app_config, resolve_csp_policy};
use netroll_app::csp::{
    CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY_REPORT_ONLY, CspHeader,
    PLAUSIBLE_DEFAULT_HOST, Policy, default_policy,
};
use netroll_app::http::{AppState, api_router};
use netroll_app::r#static::{avatar_router, spa_router};
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;
use uuid::Uuid;

/// The origin the shell is served under in these tests; the WebSocket origin
/// in `connect-src` is derived from it.
const PUBLIC_BASE_URL: &str = "https://netroll.example";
/// A self-hosted Plausible; the analytics origin in `connect-src` is derived
/// from it.
const PLAUSIBLE_HOST: &str = "https://analytics.example";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn modified_at(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Every file under `path`, or `path` itself when it is a file.
fn files_under(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    let entries = std::fs::read_dir(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    entries
        .map(|entry| entry.unwrap_or_else(|err| panic!("{}: {err}", path.display())))
        .flat_map(|entry| files_under(&entry.path()))
        .collect()
}

/// The REAL build output, and a current one. Fails closed — see the module
/// docs. Staleness is judged against the shell's inputs: a `dist/` older than
/// `frontend/index.html`, anything under `frontend/src`, or anything under
/// `frontend/public` (the FOUC guard the suite asserts on is copied from there)
/// is not "the frontend as built" and cannot prove anything about it.
fn dist_dir() -> PathBuf {
    let frontend = repo_root().join("frontend");
    let dist = frontend.join("dist");
    let index = dist.join("index.html");
    assert!(
        index.is_file(),
        "{} has no index.html — run `npm run build` in frontend/ first; this test reads the BUILT shell and never skips",
        dist.display()
    );
    let built_at = modified_at(&index);
    let newer_than_build: Vec<PathBuf> = [
        frontend.join("index.html"),
        frontend.join("src"),
        frontend.join("public"),
    ]
    .iter()
    .flat_map(|root| files_under(root))
    .filter(|file| modified_at(file) > built_at)
    .collect();
    assert!(
        newer_than_build.is_empty(),
        "{} is older than {newer_than_build:?} — run `npm run build` in frontend/ so the shell under test is the shell as built",
        index.display()
    );
    dist
}

fn app_config() -> AppConfig {
    resolve_app_config(
        Some("netroll.example".into()),
        Some(PLAUSIBLE_HOST.into()),
        None,
    )
}

fn csp_header(report_only: bool) -> CspHeader {
    CspHeader::new(&default_policy(PUBLIC_BASE_URL, &app_config()), report_only)
        .expect("the derived default is a valid header value")
}

async fn get(router: Router, path: &str) -> (StatusCode, HeaderMap, String) {
    let response = router
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn csp_values(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(CONTENT_SECURITY_POLICY)
        .iter()
        .map(|v| v.to_str().expect("ascii").to_owned())
        .collect()
}

fn assert_no_csp_of_either_name(headers: &HeaderMap, what: &str) {
    assert!(
        headers.get(CONTENT_SECURITY_POLICY).is_none(),
        "{what} must carry no Content-Security-Policy"
    );
    assert!(
        headers.get(CONTENT_SECURITY_POLICY_REPORT_ONLY).is_none(),
        "{what} must carry no Content-Security-Policy-Report-Only"
    );
}

/// Source expressions are matched case-insensitively by a browser, and
/// `Policy::parse` lower-cases directive NAMES only — so a negative check on
/// `'unsafe-inline'` must not be fooled by `'UNSAFE-INLINE'`.
fn has_source(sources: &[String], token: &str) -> bool {
    sources.iter().any(|s| s.eq_ignore_ascii_case(token))
}

// ---------------------------------------------------------------------------
// The evaluator: what the shell requires, and whether a policy permits it.
// ---------------------------------------------------------------------------

/// Where a resource comes from, as the browser will judge it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// A path-only URL, or one on the page's own origin.
    SameOrigin(String),
    /// A URL on another origin (`scheme://host[:port]`).
    Foreign(String),
    /// A URL whose scheme carries no authority — `data:`, `blob:`,
    /// `filesystem:`, `javascript:` — held as the scheme-source (`data:`) that
    /// would have to be listed to permit it.
    Scheme(String),
    /// An inline `<script>`/`<style>` body.
    Inline(String),
}

/// One thing the shell needs the policy to allow.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Requirement {
    directive: &'static str,
    source: Source,
}

fn strip_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Every `<name …>` tag: its attribute text and, for non-void tags, its body.
fn tags<'a>(html: &'a str, name: &str) -> Vec<(&'a str, Option<&'a str>)> {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let mut found = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.to_ascii_lowercase().find(&open) {
        let after_name = &rest[start + open.len()..];
        // A prefix match (`<link` vs `<linkage`) must end at whitespace, `/` or `>`.
        if !after_name.starts_with([' ', '\t', '\n', '\r', '/', '>']) {
            rest = after_name;
            continue;
        }
        let Some(gt) = after_name.find('>') else {
            break;
        };
        let attrs = after_name[..gt].trim_end_matches('/');
        let after_tag = &after_name[gt + 1..];
        let body = after_tag
            .to_ascii_lowercase()
            .find(&close)
            .map(|end| &after_tag[..end]);
        found.push((attrs, body));
        rest = match body {
            Some(b) => &after_tag[b.len() + close.len()..],
            None => after_tag,
        };
    }
    found
}

/// The value of attribute `name` in `attrs`, quoted or bare, case-insensitive
/// on the name. `None` when absent.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let mut rest = attrs.trim_start();
    while !rest.is_empty() {
        let key_end = rest
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let key = &rest[..key_end];
        rest = rest[key_end..].trim_start();
        let value = if let Some(after_eq) = rest.strip_prefix('=') {
            let after_eq = after_eq.trim_start();
            let (value, tail) = match after_eq.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let inner = &after_eq[1..];
                    let end = inner.find(q).unwrap_or(inner.len());
                    (&inner[..end], &inner[(end + 1).min(inner.len())..])
                }
                _ => {
                    let end = after_eq.find(char::is_whitespace).unwrap_or(after_eq.len());
                    (&after_eq[..end], &after_eq[end..])
                }
            };
            rest = tail.trim_start();
            Some(value.to_owned())
        } else {
            Some(String::new())
        };
        if key.eq_ignore_ascii_case(name) {
            return value;
        }
    }
    None
}

fn origin_of(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("absolute URL");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    )
}

/// The URL's scheme (`data`, `https`), lower-cased, when it has one: an ASCII
/// letter followed by letters, digits, `+`, `-` or `.`, ending at `:`. This is
/// the URL parser's own rule, so a relative path can never be mistaken for a
/// scheme — a `:` may not appear in a relative path's first segment.
fn scheme_of(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    let well_formed = first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    well_formed.then(|| scheme.to_ascii_lowercase())
}

fn classify(url: &str, page_origin: &str) -> Source {
    let url = url.trim();
    if let Some(scheme_relative) = url.strip_prefix("//") {
        // Scheme-relative resolves against the page's scheme.
        let scheme = page_origin.split("://").next().unwrap_or("https");
        return Source::Foreign(origin_of(&format!("{scheme}://{scheme_relative}")));
    }
    let Some(scheme) = scheme_of(url) else {
        return Source::SameOrigin(url.to_owned());
    };
    // A scheme with no `//` has no authority and so no origin to be "same" as:
    // `data:` and `blob:` scripts are exactly what `script-src 'self'` exists
    // to refuse, and calling them same-origin would wave them through.
    if !url[scheme.len() + 1..].starts_with("//") {
        return Source::Scheme(format!("{scheme}:"));
    }
    if origin_of(url) == origin_of(page_origin) {
        return Source::SameOrigin(url.to_owned());
    }
    Source::Foreign(origin_of(url))
}

/// Everything the shell at `index_html` requires of a policy: external
/// scripts and stylesheets by source, inline script and style blocks by body.
fn requirements(index_html: &str, page_origin: &str) -> Vec<Requirement> {
    let html = strip_comments(index_html);
    let mut out = Vec::new();
    for (attrs, body) in tags(&html, "script") {
        match attr(attrs, "src") {
            Some(src) => out.push(Requirement {
                directive: "script-src",
                source: classify(&src, page_origin),
            }),
            None => out.push(Requirement {
                directive: "script-src",
                source: Source::Inline(body.unwrap_or("").to_owned()),
            }),
        }
    }
    for (attrs, _) in tags(&html, "link") {
        let is_stylesheet = attr(attrs, "rel").is_some_and(|rel| {
            rel.split_whitespace()
                .any(|r| r.eq_ignore_ascii_case("stylesheet"))
        });
        if let (true, Some(href)) = (is_stylesheet, attr(attrs, "href")) {
            out.push(Requirement {
                directive: "style-src-elem",
                source: classify(&href, page_origin),
            });
        }
    }
    for (_, body) in tags(&html, "style") {
        out.push(Requirement {
            directive: "style-src-elem",
            source: Source::Inline(body.unwrap_or("").to_owned()),
        });
    }
    out
}

fn sha256_source(body: &str) -> String {
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(body.as_bytes()))
    )
}

/// Whether `policy` permits `requirement`, judged as a browser judges it: the
/// governing directive (with the CSP fallback chain), `'self'` for same-origin,
/// an exact origin or a scheme source for a foreign origin, the listed scheme
/// for an authority-less URL, `'unsafe-inline'` or a matching hash for an
/// inline body. A requirement NO directive governs is not permitted.
fn permits(policy: &Policy, requirement: &Requirement) -> bool {
    // A browser would let an ungoverned load through, but this evaluator's job
    // is to prove the policy GOVERNS the shell's resources: a requirement the
    // policy says nothing about is a claim the test cannot make, and answering
    // "permitted" here would let a policy with no `default-src` pass the whole
    // suite with an empty violation list. Fail closed, and name the directive.
    let Some(sources) = policy.sources(requirement.directive) else {
        return false;
    };
    if has_source(sources, "'none'") {
        return false;
    }
    let has = |token: &str| has_source(sources, token);
    match &requirement.source {
        Source::SameOrigin(_) => has("'self'") || has("*"),
        Source::Foreign(origin) => {
            let scheme_source = format!("{}:", origin.split("://").next().unwrap_or(""));
            has(origin) || has(&scheme_source) || has("*")
        }
        // `*` does not match `data:`, `blob:` or `filesystem:`, and a
        // `javascript:` URL is inline script by another name.
        Source::Scheme(scheme) => {
            has(scheme) || (scheme == "javascript:" && has("'unsafe-inline'"))
        }
        Source::Inline(body) => has("'unsafe-inline'") || has(&sha256_source(body)),
    }
}

/// The requirements `policy` does NOT permit.
///
/// Does not prove `connect-src` covers every runtime fetch or WebSocket
/// target the app might reach at runtime — only that it covers the origins
/// this suite derives statically.
fn evaluate(policy: &Policy, requirements: &[Requirement]) -> Vec<Requirement> {
    requirements
        .iter()
        .filter(|r| !permits(policy, r))
        .cloned()
        .collect()
}

/// The policy the app emitted on `GET /`, parsed.
async fn emitted_policy(dist: &Path, csp: &CspHeader) -> Policy {
    let (status, headers, _) = get(spa_router(dist, csp), "/").await;
    assert_eq!(status, StatusCode::OK);
    let values = csp_values(&headers);
    assert_eq!(
        values.len(),
        1,
        "GET / must carry exactly one Content-Security-Policy, got {values:?}"
    );
    Policy::parse(&values[0])
}

// ---------------------------------------------------------------------------
// HTML only: the header follows the content type, not the path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn html_responses_carry_exactly_one_csp_header_and_assets_carry_none() {
    let dist = dist_dir();
    let csp = csp_header(false);
    let index = std::fs::read_to_string(dist.join("index.html")).unwrap();

    for html_path in ["/", "/nets/some-client-route"] {
        let (status, headers, body) = get(spa_router(&dist, &csp), html_path).await;
        assert_eq!(status, StatusCode::OK, "{html_path}");
        assert_eq!(body, index, "{html_path} serves the shell");
        assert_eq!(
            csp_values(&headers).len(),
            1,
            "{html_path} carries exactly one Content-Security-Policy"
        );
        assert!(
            headers.get(CONTENT_SECURITY_POLICY_REPORT_ONLY).is_none(),
            "{html_path}: enforce mode sends no report-only header alongside"
        );
    }

    // The assets the shell itself names — the same ServeDir, not HTML.
    let stylesheet = requirements(&index, PUBLIC_BASE_URL)
        .into_iter()
        .find_map(|r| match (r.directive, r.source) {
            ("style-src-elem", Source::SameOrigin(href)) => Some(href),
            _ => None,
        })
        .expect("the built shell links one stylesheet");
    for asset in ["/theme-init.js", stylesheet.as_str()] {
        let (status, headers, _) = get(spa_router(&dist, &csp), asset).await;
        assert_eq!(status, StatusCode::OK, "{asset} is a real file in dist/");
        assert_no_csp_of_either_name(&headers, asset);
    }
}

/// `/docs/**` ships inside the SPA dir (the Containerfile copies the mkdocs
/// site there) and its bootstrap/mermaid shims exist only to work under
/// `script-src 'self'` — so the header must reach it. The fixture stands in
/// for the site a developer host does not have.
#[tokio::test]
async fn docs_pages_nested_under_the_spa_dir_carry_the_header() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let csp = csp_header(false);
    for path in ["/docs/index.html", "/docs/"] {
        let (status, headers, body) = get(spa_router(&fixture, &csp), path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            body.contains("NetRoll documentation"),
            "{path} is the docs page, not the shell"
        );
        assert_eq!(
            csp_values(&headers).len(),
            1,
            "{path} carries exactly one CSP"
        );
    }
}

#[tokio::test]
async fn report_only_changes_the_header_name_and_nothing_else() {
    let dist = dist_dir();
    let (_, enforced, _) = get(spa_router(&dist, &csp_header(false)), "/").await;
    let (status, reported, _) = get(spa_router(&dist, &csp_header(true)), "/").await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        reported.get(CONTENT_SECURITY_POLICY).is_none(),
        "report-only mode must not ALSO enforce"
    );
    let report_values: Vec<_> = reported
        .get_all(CONTENT_SECURITY_POLICY_REPORT_ONLY)
        .iter()
        .collect();
    assert_eq!(report_values.len(), 1);
    assert_eq!(
        report_values[0],
        enforced
            .get(CONTENT_SECURITY_POLICY)
            .expect("enforced header"),
        "same value under the other name"
    );

    // Still HTML-only.
    let (_, asset, _) = get(spa_router(&dist, &csp_header(true)), "/theme-init.js").await;
    assert_no_csp_of_either_name(&asset, "/theme-init.js in report-only mode");
}

/// `main.rs` resolves the policy as `resolve_csp_policy(CSP_POLICY)?` falling
/// back to `default_policy(…)`, and hands the result to `CspHeader::new`. The
/// two are unit-tested apart; this is the composition, on the wire: an
/// override reaches the response verbatim and nothing derived leaks into it,
/// and no override yields the derived default.
#[tokio::test]
async fn a_csp_policy_override_replaces_the_derived_default_verbatim_on_the_wire()
-> Result<(), Box<dyn std::error::Error>> {
    let dist = dist_dir();
    let config = app_config();
    let resolve = |raw: Option<&str>| -> Result<String, ConfigError> {
        Ok(resolve_csp_policy(raw.map(str::to_owned))?
            .unwrap_or_else(|| default_policy(PUBLIC_BASE_URL, &config)))
    };
    let derived = Policy::parse(&default_policy(PUBLIC_BASE_URL, &config));
    let override_value = "default-src 'none'; frame-ancestors 'none'";

    let overridden = CspHeader::new(&resolve(Some(override_value))?, false)?;
    let (status, headers, _) = get(spa_router(&dist, &overridden), "/").await;
    assert_eq!(status, StatusCode::OK);
    let values = csp_values(&headers);
    assert_eq!(
        values,
        vec![override_value.to_owned()],
        "the override is the whole header value, byte for byte"
    );
    let emitted = Policy::parse(&values[0]);
    assert_ne!(emitted, derived, "the derived default must not be emitted");
    for derived_only in ["connect-src", "script-src", "style-src-elem"] {
        assert_eq!(
            emitted.directive(derived_only),
            None,
            "{derived_only} is derived, and must not leak into an override"
        );
    }

    let defaulted = CspHeader::new(&resolve(None)?, false)?;
    let (_, headers, _) = get(spa_router(&dist, &defaulted), "/").await;
    assert_eq!(
        Policy::parse(&csp_values(&headers)[0]),
        derived,
        "no override ⇒ the derived default"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The API never sees it, proved against a real router on a real database.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SilentMailer {
    sent: Mutex<Vec<String>>,
}

impl Mailer for SilentMailer {
    fn send_magic_link<'a>(
        &'a self,
        to: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent.lock().expect("lock").push(to.to_owned());
            Ok(())
        })
    }
    fn send_email_change<'a>(
        &'a self,
        to: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent.lock().expect("lock").push(to.to_owned());
            Ok(())
        })
    }
    fn send_email_change_notice<'a>(
        &'a self,
        to: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent.lock().expect("lock").push(to.to_owned());
            Ok(())
        })
    }
}

/// A real API router over a real, migrated Postgres, plus the avatar
/// directory the composition needs. Dropping it removes the directory.
struct Harness {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    avatars: PathBuf,
}

impl Harness {
    async fn start() -> Self {
        let container = Postgres::default().start().await.expect("start postgres");
        let host = container.get_host().await.expect("host");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let pool = PgPoolOptions::new()
            .connect(&format!(
                "postgres://postgres:postgres@{host}:{port}/postgres"
            ))
            .await
            .expect("connect");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("migrate");
        let state = AppState::new(
            pool,
            Arc::new(SilentMailer::default()),
            PUBLIC_BASE_URL.into(),
        )
        .with_app_config(app_config());
        // Per-harness, not per-process: several tests in this binary run in
        // one process, and a shared directory would be removed from under a
        // sibling by the first harness to drop.
        let avatars = std::env::temp_dir().join(format!("netroll-csp-avatars-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&avatars).unwrap();
        Self {
            _container: container,
            state,
            avatars,
        }
    }

    /// `main.rs`'s composition minus the IP governors: API, avatars, then the
    /// SPA fallback, in that order.
    fn app(&self, csp: &CspHeader) -> Router {
        api_router(self.state.clone())
            .merge(avatar_router(&self.avatars))
            .merge(spa_router(&dist_dir(), csp))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Best effort: a leaked temp directory is not a test failure, and a
        // panic inside `drop` during an assertion panic would abort the run.
        let _ = std::fs::remove_dir_all(&self.avatars);
    }
}

#[tokio::test]
async fn api_and_avatar_responses_carry_no_csp_while_the_shell_does() {
    let harness = Harness::start().await;
    let csp = csp_header(false);

    // JSON reads.
    for path in ["/api/discovery", "/api/app-config", "/healthz"] {
        let (status, headers, _) = get(harness.app(&csp), path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.starts_with("application/json")),
            "{path} is JSON"
        );
        assert_no_csp_of_either_name(&headers, path);
    }
    // A problem+json rejection.
    let (status, headers, _) = get(harness.app(&csp), "/api/discovery?band=nonsense").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert_no_csp_of_either_name(&headers, "a problem+json error");
    // A missing avatar 404s from the avatar router, never via the SPA shell.
    let (status, headers, _) = get(harness.app(&csp), "/avatars/nobody.png").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_no_csp_of_either_name(&headers, "an avatar 404");
    // …and in the SAME composition the shell carries exactly one.
    let (status, headers, _) = get(harness.app(&csp), "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(csp_values(&headers).len(), 1);
}

/// The upgrade is a `101` from the API router, which the layer never wraps —
/// but the layer's content-type gate could not tell a `101` (no body, no
/// `Content-Type`) from a `304` it deliberately stamps, so the composition is
/// the only thing keeping the header off it. Proved over a real socket: only a
/// real server performs the upgrade, `oneshot` refuses it before the handler.
#[tokio::test]
async fn the_websocket_upgrade_carries_no_csp_of_either_name() {
    let harness = Harness::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let app = harness.app(&csp_header(false));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });

    // The public stream is the read capability itself: no account, no cookie.
    let url = format!("ws://{addr}/api/net-sessions/{}/live/ws", Uuid::now_v7());
    let request = url.into_client_request().expect("client request");
    let (stream, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("the handshake completes");

    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_no_csp_of_either_name(response.headers(), "the WebSocket upgrade");

    drop(stream);
    server.abort();
}

/// There is no `/api` not-found fallback: an unmatched `/api/**` path falls
/// through to the SPA fallback like any other client route and is answered
/// with the shell — HTML, so it carries the policy. Pinned here because that
/// is the real behaviour a docs claim of "API responses carry none" has to be
/// read against; changing the routing is a separate decision.
#[tokio::test]
async fn an_unmatched_api_path_is_answered_with_the_shell_and_carries_the_policy() {
    let harness = Harness::start().await;
    let index = std::fs::read_to_string(dist_dir().join("index.html")).unwrap();

    let (status, headers, body) = get(harness.app(&csp_header(false)), "/api/typo").await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/html")),
        "the SPA fallback answers an unmatched /api path"
    );
    assert_eq!(body, index, "with the shell itself");
    assert_eq!(
        csp_values(&headers).len(),
        1,
        "and so it carries the policy like any other shell response"
    );
}

// ---------------------------------------------------------------------------
// The built shell's requirements are derived and the policy satisfies them.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_built_shell_requires_nothing_the_default_policy_refuses() {
    let dist = dist_dir();
    let index = std::fs::read_to_string(dist.join("index.html")).unwrap();
    let policy = emitted_policy(&dist, &csp_header(false)).await;

    let required = requirements(&index, PUBLIC_BASE_URL);
    // The derivation must have found the shell's real resources, or an empty
    // requirement set would pass vacuously.
    let scripts = required
        .iter()
        .filter(|r| r.directive == "script-src")
        .count();
    let styles = required
        .iter()
        .filter(|r| r.directive == "style-src-elem")
        .count();
    assert!(
        scripts >= 2,
        "the built shell loads the FOUC guard and the module bundle, found {required:?}"
    );
    assert!(
        styles >= 1,
        "the built shell links its stylesheet, found {required:?}"
    );
    assert!(
        required
            .iter()
            .all(|r| matches!(r.source, Source::SameOrigin(_))),
        "every resource is same-origin: {required:?}"
    );
    // ZERO inline blocks — so `script-src 'self'` needs no hash and no nonce.
    assert!(
        !required
            .iter()
            .any(|r| matches!(r.source, Source::Inline(_))),
        "the built shell must carry no inline script or style: {required:?}"
    );

    let violations = evaluate(&policy, &required);
    assert!(
        violations.is_empty(),
        "the policy refuses the shell's own resources: {violations:?}"
    );
}

#[tokio::test]
async fn connect_src_permits_the_api_the_derived_websocket_origin_and_the_analytics_origin() {
    let policy = emitted_policy(&dist_dir(), &csp_header(false)).await;
    let connect = policy.sources("connect-src").expect("connect-src governed");
    for needed in ["'self'", "wss://netroll.example", PLAUSIBLE_HOST] {
        assert!(
            has_source(connect, needed),
            "connect-src lacks {needed}: {connect:?}"
        );
    }

    // Without a Plausible domain the analytics origin must NOT be opened.
    let quiet = CspHeader::new(
        &default_policy(PUBLIC_BASE_URL, &AppConfig::default()),
        false,
    )
    .unwrap();
    let quiet_policy = emitted_policy(&dist_dir(), &quiet).await;
    let quiet_connect = quiet_policy.sources("connect-src").unwrap();
    assert!(
        !quiet_connect
            .iter()
            .any(|s| s.to_ascii_lowercase().starts_with("https://")),
        "{quiet_connect:?}"
    );

    // http PUBLIC_BASE_URL ⇒ ws, port kept.
    let dev = CspHeader::new(
        &default_policy("http://localhost:5173", &AppConfig::default()),
        false,
    )
    .unwrap();
    let dev_connect = emitted_policy(&dist_dir(), &dev).await;
    assert!(has_source(
        dev_connect.sources("connect-src").unwrap(),
        "ws://localhost:5173"
    ));
}

#[tokio::test]
async fn the_emitted_policy_holds_its_structural_invariants() {
    let policy = emitted_policy(&dist_dir(), &csp_header(false)).await;

    let script = policy.sources("script-src").expect("script-src governed");
    assert!(!has_source(script, "'unsafe-inline'"), "{script:?}");
    // EVERY directive the policy carries, not a list this test would have to
    // keep in step with `default_policy`.
    for directive in policy.directive_names() {
        let sources = policy.directive(directive).expect("named ⇒ present");
        assert!(
            !has_source(sources, "'unsafe-eval'"),
            "'unsafe-eval' in {directive}"
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
    let exactly = |name: &str, value: &str| {
        assert_eq!(
            policy.directive(name),
            Some(&[value.to_owned()][..]),
            "{name}"
        );
    };
    exactly("object-src", "'none'");
    exactly("base-uri", "'none'");
    exactly("frame-ancestors", "'none'");
    exactly("form-action", "'self'");
    exactly("style-src-attr", "'unsafe-inline'");
}

/// The Rust default and the frontend's tracker literal are two strings that
/// have to agree, or `connect-src` refuses the app's own analytics. The backend
/// cannot import the TS constant, so it is welded on raw text here.
#[test]
fn the_plausible_default_origin_mirrors_the_frontend_literal() {
    let ts = std::fs::read_to_string(
        repo_root().join("frontend/src/features/appConfig/appConfigApi.ts"),
    )
    .expect("appConfigApi.ts");
    assert!(
        ts.contains(&format!(
            "PLAUSIBLE_DEFAULT_HOST = \"{PLAUSIBLE_DEFAULT_HOST}\""
        )),
        "frontend PLAUSIBLE_DEFAULT_HOST no longer equals {PLAUSIBLE_DEFAULT_HOST:?}"
    );
}

// ---------------------------------------------------------------------------
// The negative fixtures: the evaluator reports what the policy refuses.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_shell_with_a_foreign_script_and_an_inline_block_is_reported_as_two_violations() {
    let fixture = std::env::temp_dir().join(format!("netroll-csp-dist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&fixture);
    std::fs::create_dir_all(&fixture).unwrap();
    let inline_body = "window.x=1";
    let shell = format!(
        "<!doctype html><html><head>\
         <script src=\"/theme-init.js\"></script>\
         <script type=\"module\" crossorigin src=\"/assets/index-abc123.js\"></script>\
         <link rel=\"stylesheet\" crossorigin href=\"/assets/index-def456.css\">\
         <script src=\"https://cdn.example/x.js\"></script>\
         <script>{inline_body}</script>\
         </head><body><div id=\"root\"></div></body></html>"
    );
    std::fs::write(fixture.join("index.html"), &shell).unwrap();

    let policy = emitted_policy(&fixture, &csp_header(false)).await;
    let violations = evaluate(&policy, &requirements(&shell, PUBLIC_BASE_URL));

    assert_eq!(
        violations,
        vec![
            Requirement {
                directive: "script-src",
                source: Source::Foreign("https://cdn.example".into())
            },
            Requirement {
                directive: "script-src",
                source: Source::Inline(inline_body.into())
            },
        ],
        "exactly the foreign script and the inline block, nothing same-origin"
    );

    // And the evaluator's permit branches are real, not "everything foreign or
    // inline is a violation": each violation clears under the source that
    // legitimately covers it.
    let hash = sha256_source(inline_body);
    let permissive = Policy::parse(&format!(
        "default-src 'self'; script-src 'self' https://cdn.example {hash}"
    ));
    assert!(evaluate(&permissive, &requirements(&shell, PUBLIC_BASE_URL)).is_empty());
    let scheme_and_unsafe =
        Policy::parse("default-src 'self'; script-src 'self' https: 'unsafe-inline'");
    assert!(evaluate(&scheme_and_unsafe, &requirements(&shell, PUBLIC_BASE_URL)).is_empty());
    // A wrong hash does not clear the inline block.
    let wrong_hash = Policy::parse(&format!(
        "default-src 'self'; script-src 'self' https://cdn.example {}",
        sha256_source("window.x=2")
    ));
    assert_eq!(
        evaluate(&wrong_hash, &requirements(&shell, PUBLIC_BASE_URL)).len(),
        1
    );

    let _ = std::fs::remove_dir_all(&fixture);
}

/// A policy that governs none of the shell's directives — the shape any
/// `CSP_POLICY` override without `default-src` takes — must report every
/// requirement, not none: an empty violation list here would be the vacuous
/// green the module docs promise never to give.
#[test]
fn a_requirement_no_directive_governs_is_reported_not_waved_through() {
    let shell = "<script src=\"/theme-init.js\"></script>\
                 <link rel=\"stylesheet\" href=\"/assets/a.css\">";
    let required = requirements(shell, PUBLIC_BASE_URL);
    assert_eq!(required.len(), 2, "{required:?}");

    let ungoverned = Policy::parse("frame-ancestors 'none'; base-uri 'none'");
    assert!(
        ungoverned.sources("script-src").is_none()
            && ungoverned.sources("style-src-elem").is_none(),
        "the fixture policy must govern neither directive"
    );
    assert_eq!(evaluate(&ungoverned, &required), required);

    // The same two clear as soon as something governs them.
    assert!(evaluate(&Policy::parse("default-src 'self'"), &required).is_empty());
}

/// `data:`/`blob:`/`javascript:` scripts have no origin to be "same" as, and a
/// browser under `script-src 'self'` refuses them — so must the evaluator.
#[test]
fn an_authority_less_scheme_is_its_own_source_and_self_does_not_cover_it() {
    for (url, scheme) in [
        ("data:text/javascript,alert(1)", "data:"),
        ("DATA:text/javascript,alert(1)", "data:"),
        ("blob:https://netroll.example/0000-1111", "blob:"),
        (
            "filesystem:https://netroll.example/temporary/x.js",
            "filesystem:",
        ),
        ("javascript:alert(1)", "javascript:"),
    ] {
        assert_eq!(
            classify(url, PUBLIC_BASE_URL),
            Source::Scheme(scheme.into()),
            "{url}"
        );
    }
    // Relative and same-origin URLs are unaffected by the scheme rule.
    for url in [
        "/theme-init.js",
        "theme-init.js",
        "./assets/a.js",
        "assets/x:y.js",
        "https://netroll.example/assets/a.js",
    ] {
        assert!(
            matches!(classify(url, PUBLIC_BASE_URL), Source::SameOrigin(_)),
            "{url}"
        );
    }

    let data_script = Requirement {
        directive: "script-src",
        source: Source::Scheme("data:".into()),
    };
    let default = Policy::parse(&default_policy(PUBLIC_BASE_URL, &app_config()));
    assert!(
        !permits(&default, &data_script),
        "'self' must not cover data:"
    );
    assert!(
        !permits(&Policy::parse("script-src *"), &data_script),
        "* does not match data:"
    );
    assert!(permits(
        &Policy::parse("script-src 'self' data:"),
        &data_script
    ));
    assert!(permits(
        &Policy::parse("script-src 'self' DATA:"),
        &data_script
    ));

    let javascript_url = Requirement {
        directive: "script-src",
        source: Source::Scheme("javascript:".into()),
    };
    assert!(!permits(&default, &javascript_url));
    assert!(permits(
        &Policy::parse("script-src 'self' 'unsafe-inline'"),
        &javascript_url
    ));
}

/// The extractor itself, on the shapes Vite emits and the shapes it does not.
#[test]
fn requirements_are_derived_from_tags_not_from_comments_or_prose() {
    let html = r#"<!doctype html>
<html>
  <head>
    <!-- <script>ignored()</script> a comment naming an inline script -->
    <link rel="icon" href="/favicon.ico" />
    <LINK REL="preload stylesheet" HREF='/assets/a.css'>
    <script src="/theme-init.js"></script>
    <script type="module" crossorigin src="https://netroll.example/assets/index-1.js"></script>
    <script src=//cdn.example/bare.js></script>
    <script src="data:text/javascript,void 0"></script>
    <style>body{margin:0}</style>
  </head>
  <body><div id="root"></div></body>
</html>"#;
    let required = requirements(html, PUBLIC_BASE_URL);
    assert_eq!(
        required,
        vec![
            Requirement {
                directive: "script-src",
                source: Source::SameOrigin("/theme-init.js".into())
            },
            Requirement {
                directive: "script-src",
                source: Source::SameOrigin("https://netroll.example/assets/index-1.js".into()),
            },
            Requirement {
                directive: "script-src",
                source: Source::Foreign("https://cdn.example".into())
            },
            Requirement {
                directive: "script-src",
                source: Source::Scheme("data:".into())
            },
            Requirement {
                directive: "style-src-elem",
                source: Source::SameOrigin("/assets/a.css".into())
            },
            Requirement {
                directive: "style-src-elem",
                source: Source::Inline("body{margin:0}".into())
            },
        ]
    );
}
