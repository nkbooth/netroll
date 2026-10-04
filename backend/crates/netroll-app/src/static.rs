// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
use std::path::Path;

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

use crate::csp::CspHeader;

/// Builds the router that serves the built SPA bundle from `dist_dir`,
/// falling back to `index.html` for client-side routes, with `csp` stamped on
/// every HTML response it produces.
pub fn spa_router(dist_dir: &Path, csp: &CspHeader) -> Router {
    let index = ServeFile::new(dist_dir.join("index.html"));
    let service = ServeDir::new(dist_dir).fallback(index);
    // The layer wraps THIS router only, so the API, WS and avatar routers
    // merged beside it never see the header; inside it, the content-type gate
    // keeps the header off the JS/CSS/images the same ServeDir serves.
    Router::new()
        .fallback_service(service)
        .layer(crate::csp::html_only_layer(csp.clone()))
}

/// Serves uploaded avatars from `avatar_dir` under `/avatars`.
///
/// Mounted BEFORE the SPA fallback so a missing avatar 404s instead of being
/// answered with `index.html` (an `<img>` pointed at the SPA shell renders as a
/// broken image with no clue why). `ServeDir` resolves paths inside the root and
/// rejects traversal itself, which is the second line of defence behind the
/// domain's `is_stored_avatar_path` and the store's own filename check.
pub fn avatar_router(avatar_dir: &Path) -> Router {
    Router::new().nest_service("/avatars", ServeDir::new(avatar_dir))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::csp::CONTENT_SECURITY_POLICY;

    fn fixture_dist() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
    }

    fn csp() -> CspHeader {
        CspHeader::new("default-src 'self'", false).expect("valid policy")
    }

    async fn get_with_headers(path: &str) -> (StatusCode, HeaderMap, String) {
        let router = spa_router(&fixture_dist(), &csp());
        let response = router
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn get(path: &str) -> (StatusCode, String) {
        let (status, _, body) = get_with_headers(path).await;
        (status, body)
    }

    /// The header follows the CONTENT TYPE, not the path: the shell, the
    /// client-route fallback and a nested docs page are HTML and carry it; a
    /// script from the same directory is not and does not.
    #[tokio::test]
    async fn html_responses_carry_the_header_and_assets_do_not() {
        for html in ["/", "/nets/some-client-route", "/docs/index.html", "/docs/"] {
            let (status, headers, _) = get_with_headers(html).await;
            assert_eq!(status, StatusCode::OK, "{html}");
            assert_eq!(
                headers.get_all(CONTENT_SECURITY_POLICY).iter().count(),
                1,
                "{html} carries exactly one CSP"
            );
            assert_eq!(
                headers.get(CONTENT_SECURITY_POLICY).unwrap(),
                "default-src 'self'"
            );
        }

        let (status, headers, body) = get_with_headers("/theme-init.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("theme"),
            "the fixture script was served, not the shell"
        );
        assert!(
            headers.get(CONTENT_SECURITY_POLICY).is_none(),
            "a JS asset carries no CSP"
        );
    }

    #[tokio::test]
    async fn root_returns_200_with_the_spa_shell() {
        let (status, body) = get("/").await;

        let shell = std::fs::read_to_string(fixture_dist().join("index.html")).unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, shell);
    }

    #[tokio::test]
    async fn unknown_client_route_falls_back_to_the_spa_shell() {
        let (status, body) = get("/nets/some-client-route").await;

        let shell = std::fs::read_to_string(fixture_dist().join("index.html")).unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, shell);
    }

    /// The docs site ships inside the SPA bundle dir (`COPY --from=docs /site
    /// /app/static/docs` in the Containerfile), which only works because a real
    /// nested file beats the `index.html` fallback. Without that precedence
    /// every docs URL would silently answer with the SPA shell — a 200 showing
    /// the app's not-found screen, which is far harder to diagnose than a 404.
    #[tokio::test]
    async fn nested_static_file_wins_over_the_spa_fallback() {
        let (status, body) = get("/docs/index.html").await;

        let shell = std::fs::read_to_string(fixture_dist().join("index.html")).unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_ne!(body, shell, "docs path fell through to the SPA shell");
        assert!(body.contains("NetRoll documentation"));
    }

    /// `/docs/` (directory, trailing slash) must resolve to that directory's
    /// `index.html` rather than the SPA shell — this is the URL the nav and
    /// every cross-link actually points at.
    #[tokio::test]
    async fn nested_directory_serves_its_own_index() {
        let (status, body) = get("/docs/").await;

        let shell = std::fs::read_to_string(fixture_dist().join("index.html")).unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_ne!(body, shell, "docs directory fell through to the SPA shell");
        assert!(body.contains("NetRoll documentation"));
    }
}
